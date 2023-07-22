#[macro_use]
extern crate rocket;

use std::io::{self, Error, ErrorKind};

use rocket::{
    data::{Data, ToByteUnit},
    http::uri::Absolute,
    tokio::fs::{self, File},
    Request,
};

#[allow(clippy::declare_interior_mutable_const)]
const HOST: Absolute<'static> = uri!("http://localhost:8000");

const ID_LENGTH: usize = 16;
const LIMIT: usize = 16 * 1024 * 1024;

#[put("/<_p>", data = "<paste>")]
async fn upload_file(_p: String, paste: Data<'_>) -> io::Result<String> {
    let data = paste.open(LIMIT.bytes()).into_bytes().await?;
    if data.is_complete() {
        let mut hasher = blake3::Hasher::new();
        hasher.update_rayon(&data);
        let hash = hasher.finalize().to_string();
        let hash = hash.split_at(ID_LENGTH).0;

        fs::write(format!("upload/{hash}"), &*data).await?;
        Ok(uri!(HOST, retrieve(hash)).to_string())
    } else {
        Err(Error::new(
            ErrorKind::Other,
            "The provided data was too large.",
        ))
    }
}

#[get("/<hash>")]
async fn retrieve(hash: String) -> Option<File> {
    File::open(format!("upload/{hash}")).await.ok()
}

#[catch(404)]
fn not_found(req: &Request) -> String {
    let path = req.uri().path();
    format!("{path} not found.\n")
}

#[get("/")]
fn index() -> &'static str {
    "
 NAME
     upld.is - no bullshit pastebin
 
 USAGE
     # File Upload
     curl -T [file] upld.is
 
     # Command output
     your_command | curl upld.is -T -

     # View help info
     curl upld.is
 
 DESCRIPTION
     A simple, no bullshit command line pastebin. Pastes are created using 
     HTTP PUT requests. A url is returned, which addresses the file by its
     blake3 hash, trimmed to a certain length.
 
 INSTALL
     Add this to your shell's .rc for an easy to use alias for uploading files. 
     
     alias upld='f(){ curl upld.is -T $1; unset -f f; }; f'
    "
}

#[launch]
fn rocket() -> _ {
    rocket::build()
        .mount("/", routes![index, upload_file, retrieve])
        .register("/", catchers![not_found])
}
