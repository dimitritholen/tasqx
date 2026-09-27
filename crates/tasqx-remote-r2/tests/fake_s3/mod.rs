//! A fake R2 bucket on a `std::net::TcpListener`: just enough of the S3 API
//! for `tasqx-remote-r2` — ranged and whole GETs, conditional PUTs — plus the
//! checks a real bucket makes before it looks at a request at all.
//!
//! It does not verify signatures (that would take a second SigV4
//! implementation; the signer is held to AWS's published vectors in its unit
//! tests). It does refuse a request whose `Authorization` header is not
//! well-formed, whose `Host` is not the one it listens on, or whose body does
//! not hash to its `x-amz-content-sha256`, with a 400 that fails the test.

#![allow(dead_code)]

use std::collections::{BTreeMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use sha2::{Digest, Sha256};

pub const ACCOUNT: &str = "0123456789abcdef0123456789abcdef";
pub const BUCKET: &str = "tasqx-test";
pub const ACCESS_KEY_ID: &str = "AKIDTEST";
pub const SECRET: &str = "not-a-real-secret";
/// An access key id the bucket answers 403 AccessDenied to.
pub const DENIED_KEY_ID: &str = "AKIDDENIED";
/// A bucket that does not exist.
pub const MISSING_BUCKET: &str = "no-such-bucket";

/// One request as the bucket saw it; header names lowercase.
#[derive(Debug, Clone)]
pub struct Seen {
    pub method: String,
    pub path: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

impl Seen {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }
}

#[derive(Default)]
struct State {
    /// The object at any key: (etag without quotes, bytes).
    objects: BTreeMap<String, (String, Vec<u8>)>,
    seen: Vec<Seen>,
    /// Canned answers for the next requests: (status, S3 error code).
    forced: VecDeque<(u16, &'static str)>,
    etags: u64,
}

pub struct FakeS3 {
    pub port: u16,
    state: Arc<Mutex<State>>,
}

impl FakeS3 {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let state = Arc::new(Mutex::new(State::default()));
        let shared = Arc::clone(&state);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let state = Arc::clone(&shared);
                std::thread::spawn(move || serve(stream, port, &state));
            }
        });
        Self { port, state }
    }

    pub fn endpoint(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    /// Put an object in place, as another machine's push would have.
    pub fn seed(&self, key: &str, etag: &str, bytes: &[u8]) {
        self.state.lock().unwrap().objects.insert(
            format!("/{BUCKET}/{key}"),
            (etag.to_string(), bytes.to_vec()),
        );
    }

    /// Answer the next request with `status` and S3 error `code`.
    pub fn force(&self, status: u16, code: &'static str) {
        self.state.lock().unwrap().forced.push_back((status, code));
    }

    pub fn seen(&self) -> Vec<Seen> {
        self.state.lock().unwrap().seen.clone()
    }
}

fn serve(stream: TcpStream, port: u16, state: &Mutex<State>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();
    let mut headers = BTreeMap::new();
    loop {
        let mut h = String::new();
        reader.read_line(&mut h).unwrap();
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        let (k, v) = h.split_once(':').unwrap();
        headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
    }
    let len: usize = headers
        .get("content-length")
        .map_or(0, |v| v.parse().unwrap());
    let mut body = vec![0; len];
    reader.read_exact(&mut body).unwrap();
    let seen = Seen {
        method,
        path,
        headers,
        body,
    };
    let (status, extra, reply) = answer(&seen, port, state);
    let mut out = stream;
    let mut head = format!(
        "HTTP/1.1 {status} X\r\nContent-Length: {}\r\nConnection: close\r\n",
        reply.len()
    );
    for (k, v) in extra {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    let _ = out.write_all(head.as_bytes());
    if seen.method != "HEAD" {
        let _ = out.write_all(&reply);
    }
}

fn error(status: u16, code: &str) -> (u16, Vec<(String, String)>, Vec<u8>) {
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Error><Code>{code}</Code>\
         <Message>fake: {code}</Message></Error>"
    );
    (
        status,
        vec![("Content-Type".into(), "application/xml".into())],
        xml.into_bytes(),
    )
}

/// Why a request is malformed, if it is.
fn malformed(seen: &Seen, port: u16) -> Option<String> {
    let host = format!("127.0.0.1:{port}");
    if seen.header("host") != Some(host.as_str()) {
        return Some(format!("host {:?}, expected {host}", seen.header("host")));
    }
    let date = seen.header("x-amz-date")?;
    if date.len() != 16 || !date.ends_with('Z') || date.as_bytes()[8] != b'T' {
        return Some(format!("x-amz-date {date:?}"));
    }
    let payload = seen.header("x-amz-content-sha256")?;
    let body: String = Sha256::digest(&seen.body)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if payload != body {
        return Some("x-amz-content-sha256 does not match the body".into());
    }
    let auth = seen.header("authorization")?;
    let rest = auth.strip_prefix("AWS4-HMAC-SHA256 Credential=")?;
    let (cred, rest) = rest.split_once(", SignedHeaders=")?;
    let (signed, sig) = rest.split_once(", Signature=")?;
    let scope: Vec<&str> = cred.split('/').collect();
    if scope.len() != 5 || scope[1] != &date[..8] || scope[2..] != ["auto", "s3", "aws4_request"] {
        return Some(format!("credential scope {cred:?}"));
    }
    for must in ["host", "x-amz-content-sha256", "x-amz-date"] {
        if !signed.split(';').any(|h| h == must) {
            return Some(format!("{must} is not signed: {signed}"));
        }
    }
    for h in ["if-match", "if-none-match", "range"] {
        if seen.header(h).is_some() && !signed.split(';').any(|s| s == h) {
            return Some(format!("{h} is sent but not signed"));
        }
    }
    if sig.len() != 64 || !sig.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Some(format!("signature {sig:?}"));
    }
    None
}

fn answer(seen: &Seen, port: u16, state: &Mutex<State>) -> (u16, Vec<(String, String)>, Vec<u8>) {
    // One lock over check-and-write, so conditional PUTs are atomic.
    let mut st = state.lock().unwrap();
    st.seen.push(seen.clone());
    let well_formed = seen.header("x-amz-date").is_some()
        && seen.header("x-amz-content-sha256").is_some()
        && seen.header("authorization").is_some();
    if !well_formed {
        return error(400, "MissingSecurityHeader");
    }
    if let Some(why) = malformed(seen, port) {
        eprintln!("fake-s3: malformed request: {why}");
        return error(400, "InvalidRequest");
    }
    if let Some((status, code)) = st.forced.pop_front() {
        return error(status, code);
    }
    let auth = seen.header("authorization").unwrap();
    if auth.contains(&format!("Credential={DENIED_KEY_ID}/")) {
        return error(403, "AccessDenied");
    }
    if seen.path.starts_with(&format!("/{MISSING_BUCKET}/")) {
        return error(404, "NoSuchBucket");
    }
    let quoted = |e: &str| vec![("ETag".to_string(), format!("\"{e}\""))];
    match seen.method.as_str() {
        "GET" => match st.objects.get(&seen.path) {
            None => error(404, "NoSuchKey"),
            Some((etag, bytes)) => match seen.header("range") {
                Some("bytes=0-0") => (206, quoted(etag), bytes[..1].to_vec()),
                _ => (200, quoted(etag), bytes.clone()),
            },
        },
        "PUT" => {
            let current = st.objects.get(&seen.path).map(|(e, _)| e.clone());
            let ok = match (seen.header("if-match"), seen.header("if-none-match")) {
                (Some(want), None) => current.is_some_and(|c| format!("\"{c}\"") == want),
                (None, Some("*")) => current.is_none(),
                (None, None) => true,
                _ => false,
            };
            if !ok {
                return error(412, "PreconditionFailed");
            }
            st.etags += 1;
            let etag = format!("{:032x}", st.etags);
            st.objects
                .insert(seen.path.clone(), (etag.clone(), seen.body.clone()));
            (200, quoted(&etag), Vec::new())
        }
        _ => error(405, "MethodNotAllowed"),
    }
}
