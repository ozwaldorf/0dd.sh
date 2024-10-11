use std::io::{BufRead, Write};
use std::time::{Duration, SystemTime};

use fastly::cache::simple::CacheEntry;
use fastly::handle::BodyHandle;
use fastly::http::{Method, header};
use fastly::kv_store::InsertMode;
use fastly::{Body, Error, KVStore, Request, Response, cache, mime};
use humanize_bytes::humanize_bytes_binary;
use humantime::format_duration;
use pad::PadStr;
use serde_json::json;
use tinytemplate::TinyTemplate;

mod config {
    use std::time::Duration;

    /// Upload ID length, up to 64 bytes
    pub const ID_SIZE: usize = 8;
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

#[fastly::main]
fn main(req: Request) -> Result<Response, Error> {
    println!(
        "service version {}",
        std::env::var("FASTLY_SERVICE_VERSION").unwrap_or_default()
    );

    let mut res = match req.get_method() {
        &Method::PUT => handle_put(req)?,
        &Method::GET | &Method::HEAD => handle_get(req)?,
        _ => Response::from_status(403).with_body("invalid request"),
    };

    // Enable HSTS for 6mo
    res.set_header(header::STRICT_TRANSPORT_SECURITY, "max-age=15768000");
    // Allow CORS, deny CORP unless same origin
    res.set_header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*");
    res.set_header("Cross-Origin-Resource-Policy", "same-origin");
    // On same-origin send full referrer header, only send url for others
    res.set_header(header::REFERRER_POLICY, "strict-origin-when-cross-origin");
    // Disable external iframe embeds
    res.set_header(header::X_FRAME_OPTIONS, "SAMEORIGIN");
    // - Allow static external resources
    // - deny objects and embeds
    // - deny all scripts
    // - deny all frame ancestors
    res.set_header(
        header::CONTENT_SECURITY_POLICY,
        "default-src *; object-src 'none'; script-src 'none'; frame-ancestors 'none'",
    );

    Ok(res)
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
    let id = &base[..config::ID_SIZE];
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
    let origin_url = format!("https://{host}/{id}#integrity=blake3-{hash}");

    // Respond with download URL
    Ok(Response::from_body(url + "\n").with_header("x-origin-url", origin_url))
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

/// Handle a request to get a paste
fn handle_get(req: Request) -> Result<Response, Error> {
    let url = req.get_url();
    let host = url.host().unwrap().to_string();
    match url.path_segments().unwrap().next() {
        // Usage page
        Some("") => {
            let usage = get_usage(&host)?;
            Ok(Response::from_body(usage).with_content_type(mime::TEXT_PLAIN_UTF_8))
        },

        // Privacy policy page
        Some("privacy") => {
            const PRIVACY: &str = include_str!("privacy.txt");
            Ok(Response::from_body(PRIVACY).with_content_type(mime::TEXT_PLAIN_UTF_8))
        },

        // JSON information page
        Some("json") => {
            let kv = KVStore::open(config::KV_STORE)?.unwrap();
            let cnt = get_upload_count(&kv);
            let json = serde_json::to_string_pretty(&json!({
                "uploads": cnt,
                "id_size": config::ID_SIZE,
                "kv_ttl": format_duration(config::KV_TTL).to_string(),
                "cache_ttl": format_duration(config::CACHE_TTL).to_string()
            }))?;
            Ok(Response::from_body(json))
        },

        // Paste download
        Some(id) if id.len() == config::ID_SIZE => {
            let Ok((content, hash)) = get_paste(id) else {
                return Ok(Response::from_status(404).with_body(format!("{id} not found")));
            };

            // Respond with content
            Ok(Response::from_body(content)
                .with_header(
                    "x-origin-url",
                    format!("https://{host}/{id}#integrity=blake3-{hash}",),
                )
                .with_header(
                    // Client-side cache control, content will never change
                    header::CACHE_CONTROL,
                    "public, s-maxage=31536000, immutable",
                ))
        },

        // Unknown path
        Some(p) => Ok(Response::from_status(404).with_body(format!("{p} not found"))),
        None => unreachable!(),
    }
}

/// Handle a request to the usage page
fn get_usage(host: &str) -> Result<Body, Error> {
    const USAGE_TEMPLATE: &str = include_str!("usage.txt");

    let body = cache::simple::get_or_set_with(host.to_string(), || {
        // Compute max line
        let max_line = USAGE_TEMPLATE.lines().map(|l| l.len()).max().unwrap() + 2;

        // Build header
        let page = host.to_uppercase() + "(1)";
        let title = "User Commands"
            .pad_to_width_with_alignment(max_line - 2 * page.len(), pad::Alignment::Middle);
        let header = format!("{page}{title}{page}");

        // Build footer
        let version = std::env!("CARGO_PKG_VERSION");
        let mut footer = format!("{host} {version}");
        footer += &compile_time::date_str!().pad_to_width_with_alignment(
            max_line - footer.len() - page.len(),
            pad::Alignment::Middle,
        );
        footer += &page;

        // Get upload counter
        let kv = KVStore::open(config::KV_STORE)?.expect("kv store to exist");
        let upload_counter = get_upload_count(&kv);

        // Render template
        let mut tt = TinyTemplate::new();
        tt.add_template("usage", USAGE_TEMPLATE).unwrap();
        let rendered = tt.render(
            "usage",
            &json!({
                "header": header,
                "host": host,
                "max_size": *humanize_bytes_binary!(config::MAX_CONTENT_SIZE),
                "kv_ttl": format_duration(config::KV_TTL).to_string(),
                "cache_ttl": format_duration(config::CACHE_TTL).to_string(),
                "upload_counter": upload_counter,
                "footer": footer,
            }),
        )?;

        // Cache homepage for 5 minutes
        Ok(CacheEntry {
            value: rendered.into(),
            ttl: Duration::from_secs(5 * 60),
        })
    })?
    .expect("cache to have a body");
    Ok(body)
}

/// Get immutable content from the cache, or fallback to kv store and insert to cache.
fn get_paste(id: &str) -> Result<(BodyHandle, String), Error> {
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

        // Write content & metadata to cache
        let mut w = cache::core::insert(key.to_owned().into(), config::CACHE_TTL)
            .surrogate_keys(["get"])
            .user_metadata(meta)
            .execute()?;
        w.write_all(&content)?;
        w.finish()?;

        Ok((content.into(), hash))
    }
}
