//! No bullshit pastebin service

use std::time::Duration;

use fastly::http::{HeaderValue, Method};
use fastly::{Error, Request, Response};
use serde_json::json;
use tinytemplate::TinyTemplate;

/// Request cache ttl
const REQ_TTL: u32 = 604800; // 1 week

/// Upload ID length, up to 64 bytes
const ID_LENGTH: usize = 8;

// Content limitations
const MIN_CONTENT_SIZE: usize = 32;
const MAX_CONTENT_SIZE: usize = 24 << 20; // 24 MiB

// Key-value configuration
const KV_STORE: &str = "upldis storage";
const KV_INIT_TTL: Duration = Duration::from_secs(604800); // 1 week
const KV_TTL: Duration = Duration::from_secs(2629743); // 1 month

/// Helptext template (based on request hostname)
const HELP_TEMPLATE: &str = "\
{host}(1){padding}{host_caps}{padding}{host}(1)

 NAME
     {host} - no bullshit cli pastebin

 SYNOPSIS
     # File Upload
     curl {host} -T <file path>

     # Command output
     <command> | curl {host} -T -

     # View helptext
     curl {host}

 DESCRIPTION
     A simple, no bullshit, command line pastebin. Pastes are created
     using HTTP PUT requests, which returns a url for the content.

     Content is stored initially with a short ttl, which is extended on
     each request. Requests are also cached per region for a short time.

 EXAMPLES
     $ ps -aux | curl {host} -LT -
       https://{host}/<hash>

     $ curl {host} -LT filename.png
       https://{host}/<hash>/filename.png

 SEE ALSO
     {host} is a free service brought to you by ozwaldorf (c) 2024
     Source is available at https://github.com/ozwaldorf/upld.is
";

#[fastly::main]
fn main(req: Request) -> Result<Response, Error> {
    // Log service version
    println!(
        "FASTLY_SERVICE_VERSION: {}",
        std::env::var("FASTLY_SERVICE_VERSION").unwrap_or_else(|_| String::new())
    );

    // Filter request methods...
    match (req.get_method(), req.get_path()) {
        (&Method::GET, "/") => Ok(handle_usage(req)),
        (&Method::PUT, _) => handle_put(req),
        (&Method::GET, _) => handle_get(req),
        (_, _) => todo!(),
    }
}

fn handle_usage(req: Request) -> Response {
    println!("handling usage");

    let mut tt = TinyTemplate::new();
    tt.add_template("usage", HELP_TEMPLATE).unwrap();

    let url = req.get_url();
    let host = url.host().unwrap().to_string();

    const MAX: usize = 70 - 6;
    let hostlen = 3 * host.len();
    let padding = " ".repeat(if hostlen < MAX {
        ((MAX - (3 * host.len())) / 2).max(2)
    } else {
        2
    });

    let rendered = tt
        .render(
            "usage",
            &json!({
                "host": host,
                "host_caps": host.to_uppercase(),
                "padding": padding
            }),
        )
        .unwrap();

    Response::from_body(rendered)
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
    let filename = url.path_segments().unwrap().last();

    // Hash content and use it for the id
    let hash = blake3::hash(&body).to_hex();
    let id = &hash[..ID_LENGTH];

    let kv = fastly::kv_store::KVStore::open(KV_STORE)?.expect("kv store to exist");

    // If id does not exist already
    if kv.lookup(id).is_err() {
        // Insert to key value store with initial ttl
        kv.build_insert()
            .time_to_live(KV_INIT_TTL)
            .execute(id, body)?;
    }

    // Respond with download URL
    Ok(Response::from_body(format!(
        "https://{host}/{id}{}\n",
        if let Some(file) = filename {
            if !file.is_empty() {
                "/".to_string() + file
            } else {
                "".into()
            }
        } else {
            "".into()
        }
    )))
}

fn handle_get(mut req: Request) -> Result<Response, Error> {
    // extract id from url
    let mut segments = req.get_path().split('/').skip(1);
    let id = segments.next().expect("empty path is handled earlier");
    if id.len() != ID_LENGTH {
        return Ok(Response::from_status(404).with_body("not found"));
    }

    let kv = fastly::KVStore::open(KV_STORE)?.expect("kv store to exist");

    // Get content from key value store
    let content = match kv.lookup(id) {
        Err(_) => return Ok(Response::from_status(404).with_body("not found")),
        Ok(mut res) => res.take_body_bytes(),
    };

    // Extend time to live (by appending nothing)
    kv.build_insert()
        .time_to_live(KV_TTL)
        .mode(fastly::kv_store::InsertMode::Append)
        .execute(id, "")?;

    // Cache in POP region under key `get`
    req.set_surrogate_key(HeaderValue::from_str("get").unwrap());
    req.set_ttl(REQ_TTL);
    Ok(Response::from_body(content))
}
