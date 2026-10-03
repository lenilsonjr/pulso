//! The three routes of the ingest protocol, over HTTP/1.

use std::convert::Infallible;
use std::io::{self, Read, Write};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::Duration;

use bytes::Bytes;
use flate2::read::GzDecoder;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::header::{self, HeaderMap, HeaderValue};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

use crate::clock;
use crate::ingest::{self, ParseError};
use crate::store::Store;

/// The largest body accepted, compressed or decompressed.
const MAX_BODY: usize = 64 * 1024 * 1024;

const HEALTH: &[u8] = br#"{"status": "ok"}"#;
const NOT_FOUND: &[u8] = br#"{"error": "not found"}"#;
const UNAUTHORIZED: &[u8] = br#"{"error": "unauthorized"}"#;
const BAD_LENGTH: &[u8] = br#"{"error": "bad content length"}"#;
const BAD_GZIP: &[u8] = br#"{"error": "bad gzip body"}"#;
const TOO_LARGE: &[u8] = br#"{"error": "body too large"}"#;
const BAD_JSON: &[u8] = br#"{"error": "bad json"}"#;
const NOT_AN_ARRAY: &[u8] = br#"{"error": "body must be a json array"}"#;
const STORAGE: &[u8] = br#"{"error": "storage error"}"#;

pub struct App {
    store: Mutex<Store>,
    /// `/latest` as last committed, so reading it never waits for an ingest.
    latest: RwLock<Bytes>,
    authorization: Vec<u8>,
}

impl App {
    pub fn new(store: Store, token: &str) -> App {
        App {
            latest: RwLock::new(Bytes::from(store.latest_json())),
            store: Mutex::new(store),
            authorization: format!("Bearer {token}").into_bytes(),
        }
    }

    fn authorized(&self, headers: &HeaderMap) -> bool {
        headers
            .get(header::AUTHORIZATION)
            .is_some_and(|given| same_bytes(given.as_bytes(), &self.authorization))
    }
}

pub async fn serve(listener: TcpListener, app: Arc<App>) {
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(connection) => connection,
            Err(error) => {
                log_error(&format!("accept: {error}"));
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let app = Arc::clone(&app);
        tokio::spawn(async move {
            let service = service_fn(move |request| handle(Arc::clone(&app), peer, request));
            // A connection that fails mid-request has nobody left to tell.
            let _ = http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await;
        });
    }
}

type Reply = (Response<Full<Bytes>>, String);

async fn handle(
    app: Arc<App>,
    peer: SocketAddr,
    request: Request<Incoming>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let method = request.method().clone();
    // The whole target, query included: `/health?x=1` is not `/health`.
    let target = request
        .uri()
        .path_and_query()
        .map(|target| target.as_str().to_owned())
        .unwrap_or_default();
    let (response, note) = route(&app, &method, &target, request).await;
    log(peer, &method, &target, response.status(), &note);
    Ok(response)
}

async fn route(app: &Arc<App>, method: &Method, target: &str, request: Request<Incoming>) -> Reply {
    match (method, target) {
        (&Method::GET, "/health") => reply(200, HEALTH),
        (&Method::GET, "/latest") => {
            if !app.authorized(request.headers()) {
                return reply(401, UNAUTHORIZED);
            }
            let latest = app
                .latest
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            reply(200, latest)
        }
        (&Method::POST, "/ingest") => ingest(app, request).await,
        _ => reply(404, NOT_FOUND),
    }
}

async fn ingest(app: &Arc<App>, request: Request<Incoming>) -> Reply {
    if !app.authorized(request.headers()) {
        return reply(401, UNAUTHORIZED);
    }
    let length = declared_length(request.headers());
    if length <= 0 || length > MAX_BODY as i64 {
        return reply(if length == 0 { 400 } else { 413 }, BAD_LENGTH);
    }
    let gzip = request
        .headers()
        .get(header::CONTENT_ENCODING)
        .is_some_and(|encoding| encoding.as_bytes().eq_ignore_ascii_case(b"gzip"));
    let Ok(body) = read_body(request.into_body(), length as usize).await else {
        return reply(400, BAD_JSON);
    };

    let app = Arc::clone(app);
    tokio::task::spawn_blocking(move || ingest_body(&app, body, gzip))
        .await
        .unwrap_or_else(|_| reply(500, STORAGE))
}

fn ingest_body(app: &App, body: Vec<u8>, gzip: bool) -> Reply {
    let body = if gzip {
        let plain = match gunzip(&body) {
            Ok(plain) => plain,
            Err(Gunzip::TooLarge) => return reply(413, TOO_LARGE),
            Err(Gunzip::Invalid) => return reply(400, BAD_GZIP),
        };
        drop(body);
        plain
    } else {
        body
    };

    let stamp = clock::local_timestamp();
    let batch = match ingest::parse(&body, &stamp) {
        Ok(batch) => batch,
        Err(ParseError::BadJson) => return reply(400, BAD_JSON),
        Err(ParseError::NotAnArray) => return reply(400, NOT_AN_ARRAY),
    };
    drop(body);

    let mut store = app.store.lock().unwrap_or_else(PoisonError::into_inner);
    let counts = match store.commit(&batch, &stamp) {
        Ok(counts) => counts,
        Err(error) => {
            log_error(&format!("ingest failed: {error}"));
            return reply(500, STORAGE);
        }
    };
    if counts.new > 0 {
        *app.latest.write().unwrap_or_else(PoisonError::into_inner) =
            Bytes::from(store.latest_json());
    }
    drop(store);

    let summary = format!(
        "received={} new={} deleted={}",
        counts.received, counts.new, counts.deleted
    );
    let body = format!(
        r#"{{"received": {}, "new": {}, "deleted": {}}}"#,
        counts.received, counts.new, counts.deleted
    );
    (json(200, body), summary)
}

enum Gunzip {
    TooLarge,
    Invalid,
}

/// Only the first gzip member is read, and a stream that runs past the size
/// limit is refused rather than inflated.
fn gunzip(compressed: &[u8]) -> Result<Vec<u8>, Gunzip> {
    let mut plain = Vec::new();
    match GzDecoder::new(compressed)
        .take(MAX_BODY as u64 + 1)
        .read_to_end(&mut plain)
    {
        Ok(_) if plain.len() > MAX_BODY => Err(Gunzip::TooLarge),
        Ok(_) => Ok(plain),
        Err(_) => Err(Gunzip::Invalid),
    }
}

async fn read_body(mut body: Incoming, length: usize) -> Result<Vec<u8>, hyper::Error> {
    let mut bytes = Vec::with_capacity(length);
    while let Some(frame) = body.frame().await {
        if let Ok(data) = frame?.into_data() {
            bytes.extend_from_slice(&data);
        }
    }
    Ok(bytes)
}

/// A missing or unreadable Content-Length counts as 0.
fn declared_length(headers: &HeaderMap) -> i64 {
    headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0)
}

/// Compares without stopping at the first difference.
fn same_bytes(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0, |diff, (x, y)| diff | (x ^ y)) == 0
}

fn json(status: u16, body: impl Into<Bytes>) -> Response<Full<Bytes>> {
    let mut response = Response::new(Full::new(body.into()));
    *response.status_mut() = StatusCode::from_u16(status).expect("a valid status code");
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

fn reply(status: u16, body: impl Into<Bytes>) -> Reply {
    (json(status, body), String::new())
}

fn log(peer: SocketAddr, method: &Method, target: &str, status: StatusCode, note: &str) {
    let gap = if note.is_empty() { "" } else { " " };
    // Output nobody reads must not stop the server.
    let _ = writeln!(
        io::stdout().lock(),
        "{} {} \"{method} {target}\" {}{gap}{note}",
        clock::local_timestamp(),
        peer.ip(),
        status.as_u16()
    );
}

pub fn log_error(message: &str) {
    let _ = writeln!(
        io::stderr().lock(),
        "{} pulso-server: {message}",
        clock::local_timestamp()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_bytes_compares_whole_values() {
        assert!(same_bytes(b"Bearer secret", b"Bearer secret"));
        assert!(!same_bytes(b"Bearer secret", b"Bearer secreT"));
        assert!(!same_bytes(b"Bearer secret", b"Bearer secre"));
        assert!(!same_bytes(b"", b"x"));
        assert!(same_bytes(b"", b""));
    }

    #[test]
    fn content_length_reads_like_the_python_server() {
        let with = |value: &str| {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::CONTENT_LENGTH,
                HeaderValue::from_str(value).unwrap(),
            );
            declared_length(&headers)
        };
        assert_eq!(with("120"), 120);
        assert_eq!(with(" 7 "), 7);
        assert_eq!(with("-5"), -5);
        assert_eq!(with("abc"), 0);
        assert_eq!(with(""), 0);
        assert_eq!(declared_length(&HeaderMap::new()), 0);
    }

    #[test]
    fn gunzip_reads_one_member_and_refuses_the_rest() {
        use flate2::{Compression, write::GzEncoder};
        let gz = |data: &[u8]| {
            let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
            encoder.write_all(data).unwrap();
            encoder.finish().unwrap()
        };
        assert_eq!(gunzip(&gz(b"[1,2,3]")).ok().unwrap(), b"[1,2,3]");
        assert_eq!(gunzip(&gz(b"")).ok().unwrap(), b"");

        let mut two_members = gz(b"first");
        two_members.extend(gz(b"second"));
        assert_eq!(gunzip(&two_members).ok().unwrap(), b"first");
        let mut trailing = gz(b"data");
        trailing.extend_from_slice(b"garbage");
        assert_eq!(gunzip(&trailing).ok().unwrap(), b"data");

        let whole = gz(b"some longer text to compress, long enough to cut");
        assert!(matches!(
            gunzip(&whole[..whole.len() - 5]),
            Err(Gunzip::Invalid)
        ));
        assert!(matches!(gunzip(b"\x1f\x8bgarbage"), Err(Gunzip::Invalid)));
        assert!(matches!(gunzip(b"plain"), Err(Gunzip::Invalid)));
        let mut bad_crc = gz(b"checksummed");
        let n = bad_crc.len();
        bad_crc[n - 6] ^= 0xff;
        assert!(matches!(gunzip(&bad_crc), Err(Gunzip::Invalid)));
    }

    #[test]
    fn gunzip_refuses_a_stream_that_inflates_past_the_limit() {
        use flate2::{Compression, write::GzEncoder};
        let mut encoder = GzEncoder::new(Vec::new(), Compression::fast());
        let zeros = vec![0u8; 1 << 20];
        for _ in 0..=(MAX_BODY >> 20) {
            encoder.write_all(&zeros).unwrap();
        }
        let bomb = encoder.finish().unwrap();
        assert!(
            bomb.len() < 1 << 20,
            "a small body that would inflate past the limit"
        );
        assert!(matches!(gunzip(&bomb), Err(Gunzip::TooLarge)));
    }
}
