use std::io::{BufRead, Write};
use std::time::{Duration, SystemTime};

use fastly::handle::ResponseHandle;
use fastly::http::Method;
use fastly::kv_store::InsertMode;
use fastly::{cache, Error, KVStore, Request, Response};
use humanize_bytes::humanize_bytes_binary;
use humantime::format_duration;
use serde_json::json;
use tinytemplate::TinyTemplate;

/// Upload ID length, up to 64 bytes
const ID_LENGTH: usize = 8;
/// Minimum content size in bytes
const MIN_CONTENT_SIZE: usize = 32;
/// Maximum content size in bytes
const MAX_CONTENT_SIZE: usize = 24 << 20;
/// Fastly key-value storage name
const KV_STORE: &str = "upldis storage";
/// TTL for content
const KV_TTL: Duration = Duration::from_secs(7 * 86400);
/// Request cache ttl
const CACHE_TTL: Duration = Duration::from_secs(30 * 86400);

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
     A simple, no bullshit, command line pastebin.

     Pastes are created using HTTP PUT requests, which returns a
     determanistic URL containing a portion of the content's blake3
     hash, encoded with base58. Uploads with identical content will
     always have the same download URL.

     Content is always deleted from storage after some time.
     Once deleted, the content will remain available in regions that
     have it cached still.

 NOTES
     * Maximum file size   :  {max_size}
     * Storage TTL         :  {kv_ttl}
     * Regional cache TTL  :  {cache_ttl}
     * All time uploads    :  {upload_counter}

 EXAMPLES
     $ echo 'testing' | curl {host} -LT -
       https://{host}/deadbeef

     $ curl https://{host}/deadbeef
       testing

 CAVEATS
     Respect for intellectual property rights is paramount.
     Users must not post any material that infringes on the copyright
     or other intellectual property rights of others.
     This includes unauthorized copies of software, music, videos,
     and other copyrighted materials.

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

    // Filter request methods...
    match (req.get_method(), req.get_path()) {
        (&Method::GET, "/") => handle_usage(req),
        (&Method::GET, "/metrics") => {
            let kv = KVStore::open(KV_STORE)?.unwrap();
            if let Ok(mut metrics) = kv.lookup("_upload_metrics") {
                Ok(Response::from_handles(
                    ResponseHandle::new(),
                    metrics.take_body().into_handle(),
                ))
            } else {
                Ok(Response::new())
            }
        }
        (&Method::GET, _) => handle_get(req),
        (&Method::PUT, _) => handle_put(req),
        _ => Ok(Response::from_status(403).with_body("invalid request")),
    }
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

    let kv = KVStore::open(KV_STORE)?.expect("kv store to exist");
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
            "max_size": *humanize_bytes_binary!(MAX_CONTENT_SIZE),
            "kv_ttl": format_duration(KV_TTL).to_string(),
            "cache_ttl": format_duration(CACHE_TTL).to_string(),
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
    if body.len() < MIN_CONTENT_SIZE {
        return Ok(Response::from_status(400).with_body_text_plain("content too small"));
    }
    if body.len() > MAX_CONTENT_SIZE {
        return Ok(Response::from_status(413).with_body_text_plain("content too large"));
    }

    let url = req.get_url();
    let host = url.host().unwrap().to_string();
    let filename = url
        .path_segments()
        .unwrap()
        .last()
        .and_then(|v| (!v.is_empty()).then_some(v));

    // Hash content and use base58 for the id
    let hash = bs58::encode(blake3::hash(&body).as_bytes()).into_string();
    let id = &hash[..ID_LENGTH];
    let key = &format!("file_{id}");

    // Insert content to key value store
    let kv = KVStore::open(KV_STORE)?.expect("kv store to exist");
    if kv.lookup(key).is_err() {
        kv.build_insert().time_to_live(KV_TTL).execute(key, body)?;
        track_upload(&kv, id, filename.unwrap_or("undefined"))?;
    }

    println!("put {key} in storage");

    // Respond with download URL
    Ok(Response::from_body(format!(
        "https://{host}/{id}{}\n",
        filename.map(|v| "/".to_string() + v).unwrap_or_default()
    )))
}

fn handle_get(req: Request) -> Result<Response, Error> {
    // Extract id from url
    let mut segments = req.get_path().split('/').skip(1);
    let id = segments.next().expect("empty path is handled earlier");
    if id.len() != ID_LENGTH {
        return Ok(Response::from_status(404).with_body("not found"));
    }
    let key = &format!("file_{id}");

    // Try to find content in cache
    if let Some(found) = cache::core::lookup(key.to_owned().into()).execute()? {
        let body_handle = found.to_stream()?.into_handle();
        let res = Response::from_handles(ResponseHandle::new(), body_handle);
        return Ok(res);
    }

    // Otherwise, get content from key value store (origin)
    let kv = KVStore::open(KV_STORE)?.expect("kv store to exist");
    let content = match kv.lookup(key) {
        Err(_) => return Ok(Response::from_status(404).with_body("not found")),
        Ok(mut res) => res.take_body_bytes(),
    };

    // Write content to cache
    let mut w = cache::core::insert(key.to_owned().into(), CACHE_TTL)
        .surrogate_keys(["get"])
        .execute()?;
    w.write_all(&content)?;
    w.finish()?;

    // Respond with content
    Ok(Response::from_body(content))
}

/// Key to store upload metrics under
const UPLOAD_METRICS_KEY: &str = "_upload_metrics";

/// Get upload count from the metadata, or fallback to the number of metric lines.
fn get_upload_count(kv: &KVStore) -> usize {
    kv.lookup(UPLOAD_METRICS_KEY)
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
            UPLOAD_METRICS_KEY,
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
