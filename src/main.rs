use std::io::{BufRead, Write};
use std::time::SystemTime;

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
const HELP_TEMPLATE: &str = "\
{host}(1){padding}{host_caps}{padding}{host}(1)

 NAME
     {host} - no bullshit command line pastebin

 SYNOPSIS
     # View helptext
     curl {host} -L

     # Upload file
     curl {host} -LT <file path>

     # Upload command output
     <command> | curl {host} -LT -

 DESCRIPTION
     A simple, no bullshit, tamper-proof command line pastebin.

     Pastes are created using HTTP PUT requests, which returns a
     determanistic paste URL. Filenames are ignored in the URL
     and can be modified or removed entirely.

     Paste downloads can be verified by doing the following:
         1) Get the checksum from the HTTP header `x-content-hash`

         2) Verify the checksum by re-encoding with base58 and
            ensuring it matches the slice in the paste URL

         3) Verify the recieved content by hashing with blake3 and
            comparing against the checksum.

     Pastes are always deleted from storage after some time. Once
     deleted, the content will remain available in regions that have
     it cached still. However, content can always be re-uploaded to
     the same paste URL.

 NOTES
     * Maximum file size    :   {max_size}
     * Storage TTL          :   {kv_ttl}
     * Regional cache TTL   :   {cache_ttl}
     * All time uploads     :   {upload_counter}

 EXAMPLES
     $ echo 'testing' | curl {host} -LT -
       https://{host}/deadbeef

     $ curl https://{host}/deadbeef
       testing

 CAVEATS
     Respect for intellectual property rights is paramount. Users
     must not post any material that infringes on the copyright or
     other intellectual property rights of others. This includes
     unauthorized copies of software, music, videos, and other
     copyrighted materials.

 COPYRIGHT
     Ossian Mapes (c) 2024, MIT

 SEE ALSO
     https://github.com/ozwaldorf/upld.is
";

#[fastly::main]
fn main(req: Request) -> Result<Response, Error> {
    // Log service version
    println!(
        "service version {}",
        std::env::var("FASTLY_SERVICE_VERSION").unwrap_or_default()
    );

    let res = match (req.get_method(), req.get_path()) {
        // Handle usage page
        (&Method::GET, "/") => handle_usage(req)?,
        // Handle getting content
        (&Method::GET, _) => handle_get(req)?.with_header(
            // Client-side cache control, content will never change
            header::CACHE_CONTROL,
            "public, s-maxage=31536000, immutable",
        ),
        // Handle uploading content
        (&Method::PUT, _) => handle_put(req)?,
        // Fallback to forbidden error
        _ => Response::from_status(403).with_body("invalid request"),
    };

    // enable hsts, cross origin sharing, disable iframe embeds
    Ok(res
        .with_header(header::STRICT_TRANSPORT_SECURITY, "max-age=2592000")
        .with_header(header::REFERRER_POLICY, "origin-when-cross-origin")
        .with_header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .with_header(header::X_FRAME_OPTIONS, "SAMEORIGIN"))
}

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
    Ok(Response::from_body(rendered))
}

fn handle_put(mut req: Request) -> Result<Response, Error> {
    // Check request body
    if !req.has_body() {
        return Ok(Response::from_status(400).with_body_text_plain("missing upload body"));
    }
    let body = req.take_body_bytes();
    if body.len() < config::MIN_CONTENT_SIZE {
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

    // Respond with download URL
    Ok(Response::from_body(format!(
        "https://{host}/{id}{}\n",
        filename.map(|v| "/".to_string() + v).unwrap_or_default()
    ))
    .with_header("x-content-hash", hash.to_string()))
}

fn handle_get(req: Request) -> Result<Response, Error> {
    // Extract id from url
    let mut segments = req.get_path().split('/').skip(1);
    let id = segments.next().expect("empty path is handled earlier");
    if id.len() != config::ID_LENGTH {
        return Ok(Response::from_status(404).with_body("not found"));
    }
    let key = &format!("file_{id}");

    // Try to find content in cache
    if let Some(found) = cache::core::lookup(key.to_owned().into()).execute()? {
        return Ok(Response::new()
            .with_header("x-content-hash", found.user_metadata().as_ref())
            .with_body(found.to_stream()?.into_handle()));
    }

    // Otherwise, get content from key value store (origin)
    let kv = KVStore::open(config::KV_STORE)?.expect("kv store to exist");
    let (meta, content) = match kv.lookup(key) {
        Err(_) => return Ok(Response::from_status(404).with_body("not found")),
        Ok(mut res) => (res.metadata().unwrap(), res.take_body_bytes()),
    };

    // Start building response with content hash header. Separated to avoid some clones
    let res = Response::new().with_header("x-content-hash", meta.as_ref());

    // Write content to cache
    let mut w = cache::core::insert(key.to_owned().into(), config::CACHE_TTL)
        .surrogate_keys(["get"])
        .user_metadata(meta)
        .execute()?;
    w.write_all(&content)?;
    w.finish()?;

    // Respond with content
    Ok(res.with_body(content))
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
