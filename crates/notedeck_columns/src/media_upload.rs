#![cfg_attr(target_os = "android", allow(dead_code, unused_variables))]

use std::path::PathBuf;

use base64::{prelude::BASE64_URL_SAFE, Engine};
use ehttp::Request;
use nostrdb::{Note, NoteBuilder};
use notedeck::SupportedMimeType;
use poll_promise::Promise;
use sha2::{Digest, Sha256};
use url::Url;

use crate::Error;
use notedeck::media::images::fetch_binary_from_disk;

pub const NOSTR_BUILD_URL: fn() -> Url = || Url::parse("http://nostr.build").unwrap();
const NIP96_WELL_KNOWN: &str = ".well-known/nostr/nip96.json";

fn get_upload_url(nip96_url: Url) -> Promise<Result<String, Error>> {
    let request = Request::get(nip96_url);
    let (sender, promise) = Promise::new();

    ehttp::fetch(request, move |response| {
        let result = match response {
            Ok(resp) => {
                if resp.status == 200 {
                    if let Some(text) = resp.text() {
                        get_api_url_from_json(text)
                    } else {
                        Err(Error::Generic(
                            "ehttp::Response payload is not text".to_owned(),
                        ))
                    }
                } else {
                    Err(Error::Generic(format!(
                        "ehttp::Response status: {}",
                        resp.status
                    )))
                }
            }
            Err(e) => Err(Error::Generic(e)),
        };

        sender.send(result);
    });

    promise
}

fn get_api_url_from_json(json: &str) -> Result<String, Error> {
    match serde_json::from_str::<serde_json::Value>(json) {
        Ok(json) => {
            if let Some(url) = json
                .get("api_url")
                .and_then(|url| url.as_str())
                .map(|url| url.to_string())
            {
                Ok(url)
            } else {
                Err(Error::Generic(
                    "api_url key not found in ehttp::Response".to_owned(),
                ))
            }
        }
        Err(e) => Err(Error::Generic(e.to_string())),
    }
}

fn get_upload_url_from_provider(mut provider_url: Url) -> Promise<Result<String, Error>> {
    provider_url.set_path(NIP96_WELL_KNOWN);
    get_upload_url(provider_url)
}

pub fn get_nostr_build_upload_url() -> Promise<Result<String, Error>> {
    get_upload_url_from_provider(NOSTR_BUILD_URL())
}

fn create_nip98_note(seckey: &[u8; 32], upload_url: String, payload_hash: String) -> Note {
    NoteBuilder::new()
        .kind(27235)
        .start_tag()
        .tag_str("u")
        .tag_str(&upload_url)
        .start_tag()
        .tag_str("method")
        .tag_str("POST")
        .start_tag()
        .tag_str("payload")
        .tag_str(&payload_hash)
        .sign(seckey)
        .build()
        .expect("build note")
}

fn create_nip96_request(
    upload_url: &str,
    media_path: MediaPath,
    file_contents: Vec<u8>,
    nip98_base64: &str,
) -> ehttp::Request {
    let boundary = "----boundary";

    let mut body = format!(
        "--{}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{}\"\r\nContent-Type: {}\r\n\r\n",
        boundary, media_path.file_name, media_path.media_type.to_mime()
    )
    .into_bytes();
    body.extend(file_contents);
    body.extend(format!("\r\n--{boundary}--\r\n").as_bytes());

    let headers = ehttp::Headers::new(&[
        (
            "Content-Type",
            format!("multipart/form-data; boundary={boundary}").as_str(),
        ),
        ("Authorization", format!("Nostr {nip98_base64}").as_str()),
    ]);

    Request {
        method: "POST".to_string(),
        url: upload_url.to_string(),
        headers,
        body,
    }
}

fn sha256_hex(contents: &Vec<u8>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(contents);
    let hash = hasher.finalize();
    hex::encode(hash)
}

pub fn nip96_upload(
    seckey: [u8; 32],
    upload_url: String,
    media_path: MediaPath,
) -> Promise<Result<Nip94Event, Error>> {
    let bytes_res = fetch_binary_from_disk(media_path.full_path.clone());

    let file_bytes = match bytes_res {
        Ok(bytes) => bytes,
        Err(e) => {
            return Promise::from_ready(Err(Error::Generic(format!(
                "could not read contents of file to upload: {e}"
            ))));
        }
    };

    internal_nip96_upload(seckey, upload_url, media_path, file_bytes)
}

pub fn nostrbuild_nip96_upload(
    seckey: [u8; 32],
    media_path: MediaPath,
) -> Promise<Result<Nip94Event, Error>> {
    let (sender, promise) = Promise::new();
    std::thread::spawn(move || {
        let upload_url = match get_nostr_build_upload_url().block_and_take() {
            Ok(url) => url,
            Err(e) => {
                sender.send(Err(Error::Generic(format!(
                    "could not get nostrbuild upload url: {e}"
                ))));
                return;
            }
        };

        let res = nip96_upload(seckey, upload_url, media_path).block_and_take();
        sender.send(res);
    });
    promise
}

fn internal_nip96_upload(
    seckey: [u8; 32],
    upload_url: String,
    media_path: MediaPath,
    file_contents: Vec<u8>,
) -> Promise<Result<Nip94Event, Error>> {
    let file_hash = sha256_hex(&file_contents);
    let nip98_note = create_nip98_note(&seckey, upload_url.to_owned(), file_hash);

    let nip98_base64 = match nip98_note.json() {
        Ok(json) => BASE64_URL_SAFE.encode(json),
        Err(e) => return Promise::from_ready(Err(Error::Generic(e.to_string()))),
    };

    let request = create_nip96_request(&upload_url, media_path, file_contents, &nip98_base64);

    let (sender, promise) = Promise::new();

    ehttp::fetch(request, move |response| {
        let maybe_uploaded_media = match response {
            Ok(response) => {
                if response.ok {
                    match String::from_utf8(response.bytes.clone()) {
                        Ok(str_response) => find_nip94_ev_in_json(str_response),
                        Err(e) => Err(Error::Generic(e.to_string())),
                    }
                } else {
                    Err(Error::Generic(format!(
                        "ehttp Response was unsuccessful. Code {} with message: {}",
                        response.status, response.status_text
                    )))
                }
            }
            Err(e) => Err(Error::Generic(e)),
        };

        sender.send(maybe_uploaded_media);
    });

    promise
}

fn find_nip94_ev_in_json(json: String) -> Result<Nip94Event, Error> {
    match serde_json::from_str::<serde_json::Value>(&json) {
        Ok(v) => {
            let tags = v["nip94_event"]["tags"].clone();
            let content = v["nip94_event"]["content"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            match serde_json::from_value::<Vec<Vec<String>>>(tags) {
                Ok(tags) => Nip94Event::from_tags_and_content(tags, content)
                    .map_err(|e| Error::Generic(e.to_owned())),
                Err(e) => Err(Error::Generic(e.to_string())),
            }
        }
        Err(e) => Err(Error::Generic(e.to_string())),
    }
}

#[derive(Debug)]
pub struct MediaPath {
    full_path: PathBuf,
    file_name: String,
    media_type: SupportedMimeType,
}

impl MediaPath {
    pub fn new(path: PathBuf) -> Result<Self, Error> {
        if let Some(ex) = path.extension().and_then(|f| f.to_str()) {
            let media_type = SupportedMimeType::from_extension(ex)?;
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(&format!("file.{ex}"))
                .to_owned();

            Ok(MediaPath {
                full_path: path,
                file_name,
                media_type,
            })
        } else {
            Err(Error::Generic(format!(
                "{path:?} does not have an extension"
            )))
        }
    }
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct Nip94Event {
    pub url: String,
    pub ox: Option<String>,
    pub x: Option<String>,
    pub media_type: Option<String>,
    pub dimensions: Option<(u32, u32)>,
    pub blurhash: Option<String>,
    pub thumb: Option<String>,
    pub content: String,
}

impl Nip94Event {
    pub fn new(url: String, width: u32, height: u32) -> Self {
        Self {
            url,
            ox: None,
            x: None,
            media_type: None,
            dimensions: Some((width, height)),
            blurhash: None,
            thumb: None,
            content: String::new(),
        }
    }
}

const URL: &str = "url";
const OX: &str = "ox";
const X: &str = "x";
const M: &str = "m";
const DIM: &str = "dim";
const BLURHASH: &str = "blurhash";
const THUMB: &str = "thumb";

impl Nip94Event {
    fn from_tags_and_content(
        tags: Vec<Vec<String>>,
        content: String,
    ) -> Result<Self, &'static str> {
        let mut url = None;
        let mut ox = None;
        let mut x = None;
        let mut media_type = None;
        let mut dimensions = None;
        let mut blurhash = None;
        let mut thumb = None;

        for tag in tags {
            match tag.as_slice() {
                [key, value] if key == URL => url = Some(value.to_string()),
                [key, value] if key == OX => ox = Some(value.to_string()),
                [key, value] if key == X => x = Some(value.to_string()),
                [key, value] if key == M => media_type = Some(value.to_string()),
                [key, value] if key == DIM => {
                    if let Some((w, h)) = value.split_once('x') {
                        if let (Ok(w), Ok(h)) = (w.parse::<u32>(), h.parse::<u32>()) {
                            dimensions = Some((w, h));
                        }
                    }
                }
                [key, value] if key == BLURHASH => blurhash = Some(value.to_string()),
                [key, value] if key == THUMB => thumb = Some(value.to_string()),
                _ => {}
            }
        }

        Ok(Self {
            url: url.ok_or("Missing url")?,
            ox,
            x,
            media_type,
            dimensions,
            blurhash,
            thumb,
            content,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf, str::FromStr};

    use enostr::FullKeypair;

    use crate::media_upload::{
        get_upload_url_from_provider, nostrbuild_nip96_upload, MediaPath, NOSTR_BUILD_URL,
    };

    use super::internal_nip96_upload;

    #[test]
    fn test_nostrbuild_upload_url() {
        let promise = get_upload_url_from_provider(NOSTR_BUILD_URL());

        let url = promise.block_until_ready();

        assert!(url.is_ok());
    }

    #[test]
    #[ignore] // this test should not run automatically since it sends data to a real server
    fn test_internal_nip96() {
        // just a random image to test image upload
        let file_path = PathBuf::from_str("../../../assets/damus_rounded_80.png").unwrap();
        let media_path = MediaPath::new(file_path).unwrap();
        let img_bytes = include_bytes!("../../../assets/damus_rounded_80.png");
        let promise = get_upload_url_from_provider(NOSTR_BUILD_URL());
        let kp = FullKeypair::generate();
        println!("Using pubkey: {:?}", kp.pubkey);

        if let Ok(upload_url) = promise.block_until_ready() {
            let promise = internal_nip96_upload(
                kp.secret_key.secret_bytes(),
                upload_url.to_string(),
                media_path,
                img_bytes.to_vec(),
            );
            let res = promise.block_until_ready();
            assert!(res.is_ok())
        } else {
            panic!()
        }
    }

    #[tokio::test]
    #[ignore] // this test should not run automatically since it sends data to a real server
    async fn test_nostrbuild_nip96() {
        // just a random image to test image upload
        let file_path =
            fs::canonicalize(PathBuf::from_str("../../assets/damus_rounded_80.png").unwrap())
                .unwrap();
        let media_path = MediaPath::new(file_path).unwrap();
        let kp = FullKeypair::generate();
        println!("Using pubkey: {:?}", kp.pubkey);

        let promise = nostrbuild_nip96_upload(kp.secret_key.secret_bytes(), media_path);

        let out = promise.block_and_take();
        assert!(out.is_ok());
    }
}
