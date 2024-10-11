use std::io::{BufRead, Write};
use std::time::SystemTime;

use fastly::handle::BodyHandle;
use fastly::http::{header, Method};
use fastly::kv_store::InsertMode;
use fastly::{cache, Error, KVStore, Request, Response};
use humanize_bytes::humanize_bytes_binary;
use humantime::format_duration;
use serde_json::json;
use tinytemplate::TinyTemplate;

mod config {
    use std::time::Duration;

    /// Upload ID length, up to 64 bytes
    pub const ID_LENGTH: usize = 8;
    /// Minimum content size in bytes
    pub const MIN_CONTENT_SIZE: usize = 32;
    /// Maximum content size in bytes
    pub const MAX_CONTENT_SIZE: usize = 24 << 20;
    /// Fastly key-value storage name
    pub const KV_STORE: &str = "upldis storage";
    /// TTL for content
    pub const KV_TTL: Duration = Duration::from_secs(7 * 86400);
    /// Request cache ttl
    pub const CACHE_TTL: Duration = Duration::from_secs(30 * 86400);
    /// Key to store upload metrics under
    pub const UPLOAD_METRICS_KEY: &str = "_upload_metrics";
}

/// Helptext template (based on request hostname)
const HELP_TEMPLATE: &str = include_str!("usage.txt");
const PRIVACY_TEMPLATE: &str = include_str!("privacy.txt");

#[fastly::main]
fn main(req: Request) -> Result<Response, Error> {
    // Log service version
    println!(
        "service version {}",
        std::env::var("FASTLY_SERVICE_VERSION").unwrap_or_default()
    );

    Ok(match (req.get_method(), req.get_path()) {
        (&Method::GET | &Method::HEAD, "/") => handle_usage(req)?,
        (&Method::GET, "/privacy") => handle_privacy(),
        (&Method::GET, _) => handle_get(req)?,
        (&Method::HEAD, _) => handle_get(req)?,
        (&Method::PUT, _) => handle_put(req)?,
        _ => Response::from_status(403).with_body("invalid request"),
    }
    // enable hsts, cross origin sharing, disable iframe embeds
    .with_header(header::STRICT_TRANSPORT_SECURITY, "max-age=2592000")
    .with_header(header::REFERRER_POLICY, "origin-when-cross-origin")
    .with_header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
    .with_header(header::X_FRAME_OPTIONS, "SAMEORIGIN"))
}

/// Handle a request to the usage page
fn handle_usage(req: Request) -> Result<Response, Error> {
    // Get hostname from request
    let url = req.get_url();
    let host = url.host().unwrap().to_string();

    // Create padding for header
    const MAX: usize = 70 - 6;
    let hostlen = 3 * host.len();
    let padding = " ".repeat(if hostlen < MAX {
        ((MAX - (3 * host.len())) / 2 + 1).max(2)
    } else {
        2
    });

    let kv = KVStore::open(config::KV_STORE)?.expect("kv store to exist");
    let upload_counter = get_upload_count(&kv);

    // Render template
    let mut tt = TinyTemplate::new();
    tt.add_template("usage", HELP_TEMPLATE).unwrap();
    let rendered = tt.render(
        "usage",
        &json!({
            "host": host,
            "host_caps": host.to_uppercase(),
            "padding": padding,
            "max_size": *humanize_bytes_binary!(config::MAX_CONTENT_SIZE),
            "kv_ttl": format_duration(config::KV_TTL).to_string(),
            "cache_ttl": format_duration(config::CACHE_TTL).to_string(),
            "upload_counter": upload_counter
        }),
    )?;

    // Respond with rendered helptext
    Ok(Response::new().with_body_text_plain(&rendered))
}

/// Handle a request to the privacy policy page
fn handle_privacy() -> Response {
    Response::new().with_body_text_plain(PRIVACY_TEMPLATE)
}

/// Handle a request to put a paste into storage
fn handle_put(mut req: Request) -> Result<Response, Error> {
    // Check request body
    if !req.has_body() {
        return Ok(Response::from_status(400).with_body_text_plain("missing upload body"));
    }
    let body = req.take_body_bytes();
    if body.len() < config::MIN_CONTENT_SIZE && body != b"testing" {
        return Ok(Response::from_status(400).with_body_text_plain("content too small"));
    }
    if body.len() > config::MAX_CONTENT_SIZE {
        return Ok(Response::from_status(413).with_body_text_plain("content too large"));
    }

    let url = req.get_url();
    let host = url.host().unwrap().to_string();
    let filename = url
        .path_segments()
        .unwrap()
        .last()
        .and_then(|v| (!v.is_empty()).then_some(v));

    // Hash content and use a section of base58 encoding for the id
    let hash = blake3::hash(&body);
    let base = bs58::encode(hash.as_bytes()).into_string();
    let id = &base[..config::ID_LENGTH];
    let key = &format!("file_{id}");

    // Insert content to key value store
    let kv = KVStore::open(config::KV_STORE)?.expect("kv store to exist");
    if kv.lookup(key).is_err() {
        kv.build_insert()
            .metadata(&hash.to_hex())
            .time_to_live(config::KV_TTL)
            .execute(key, body)?;
        track_upload(&kv, id, filename.unwrap_or("undefined"))?;
    }

    println!("put {key} in storage");

    let url = format!(
        "https://{host}/{id}{}",
        filename.map(|v| "/".to_string() + v).unwrap_or_default()
    );
    let origin_url = format!("{url}#integrity=blake3-{hash}");

    // Respond with download URL
    Ok(Response::from_body(url + "\n")
        .with_header("x-content-hash", hash.to_string())
        .with_header("x-origin-url", origin_url))
}

/// Handle a request to get a paste
fn handle_get(req: Request) -> Result<Response, Error> {
    // Extract id from url
    let mut segments = req.get_path().split('/').skip(1);
    let id = segments.next().expect("empty path is handled earlier");
    if id.len() != config::ID_LENGTH {
        return Ok(Response::from_status(404).with_body("not found"));
    }

    let Ok((content, hash)) = get_cached_content(id) else {
        return Ok(Response::from_status(404).with_body("not found"));
    };

    let mut origin_url = req.get_url().clone();
    origin_url.set_fragment(Some(&format!("integrity=blake3-{hash}")));

    // Respond with content
    Ok(Response::from_body(content)
        .with_header("x-content-hash", hash)
        .with_header("x-origin-url", origin_url)
        .with_header(
            // Client-side cache control, content will never change
            header::CACHE_CONTROL,
            "public, s-maxage=31536000, immutable",
        ))
}

/// Get immutable content from the cache, or fallback to kv store and insert to cache.
fn get_cached_content(id: &str) -> Result<(BodyHandle, String), Error> {
    let key = "file_".to_string() + id;

    // Try to find content in cache
    if let Some(found) = cache::core::lookup(key.clone().into()).execute()? {
        let meta = found.user_metadata();
        let hash = String::from_utf8(meta.to_vec()).unwrap();

        Ok((found.to_stream()?.into_handle(), hash))
    } else {
        // Otherwise, get content from key value store (origin)
        let kv = KVStore::open(config::KV_STORE)?.expect("kv store to exist");
        let mut res = kv.lookup(&key)?;
        let meta = res.metadata().unwrap();
        let hash = String::from_utf8(meta.to_vec()).unwrap();
        let content = res.take_body_bytes();

        // Write content to cache
        let mut w = cache::core::insert(key.to_owned().into(), config::CACHE_TTL)
            .surrogate_keys(["get"])
            .user_metadata(meta)
            .execute()?;
        w.write_all(&content)?;
        w.finish()?;

        Ok((content.into(), hash))
    }
}

/// Get upload count from the metadata, or fallback to the number of metric lines.
fn get_upload_count(kv: &KVStore) -> usize {
    kv.lookup(config::UPLOAD_METRICS_KEY)
        .ok()
        .map(|mut v| {
            v.metadata()
                // try and parse from metadata
                .and_then(|m| String::from_utf8_lossy(&m).parse().ok())
                // otherwise, count number of metric lines
                .unwrap_or(v.take_body_bytes().lines().count())
        })
        .unwrap_or_default()
}

/// Append the key and a timestamp to the metrics
fn track_upload(kv: &KVStore, id: &str, file: &str) -> Result<(), Error> {
    let new_count = get_upload_count(kv) + 1;
    kv.build_insert()
        .mode(InsertMode::Append)
        .metadata(&new_count.to_string())
        .execute(
            config::UPLOAD_METRICS_KEY,
            format!(
                "{:?} , {id} , {file}\n",
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis()
            ),
        )?;
    Ok(())
}
