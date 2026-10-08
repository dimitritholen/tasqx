//! The hand-rolled HTTP/1.1 reader and writer both loopback listeners share
//! (#637): the OTLP receiver ([`crate::otlp`]) and the board ([`crate::board`]).
//!
//! Deliberately small and runtime-free (DESIGN §2): a request line, headers, a
//! `Content-Length` body, all capped, over a blocking `TcpStream`. No `http`
//! crate, no async. The two listeners keep their own accept loops and their own
//! policy, because their threat models differ (OTLP is unauthenticated
//! telemetry ingress; the board authenticates). Only the bytes-on-the-wire
//! parts live here.

use std::io::{self, BufRead, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

/// Cap on the header block (request line + headers). Well past any real client,
/// small enough that a peer dribbling headers cannot grow memory unbounded.
pub const MAX_HEADER_BYTES: usize = 64 * 1024;

/// Per-read/write socket timeout. This is a PER-READ timeout — it resets on
/// every byte received — so on its own it only catches a *fully* idle peer, not
/// a slow-drip one; [`REQUEST_DEADLINE`] bounds the latter.
pub const IO_TIMEOUT: Duration = Duration::from_secs(5);

/// Whole-request deadline from the moment a connection is accepted. Unlike
/// [`IO_TIMEOUT`] (which resets on every byte and so is defeated by a peer
/// dribbling one byte just inside the timeout — a slowloris), this is a hard
/// ceiling on the total time one peer may spend sending a request, regardless
/// of how it paces its bytes.
pub const REQUEST_DEADLINE: Duration = Duration::from_secs(10);

/// Make an accepted socket blocking, with read and write timeouts.
///
/// An accept loop's listener is nonblocking so it can poll a shutdown flag, and
/// an ACCEPTED socket inherits that flag on more platforms than it does not:
/// BSD-derived kernels (macOS) hand it down, and so does Windows, whose
/// accept() copies the listening socket's properties. Linux is the exception,
/// which is exactly why this was invisible for so long — the one platform that
/// does NOT inherit is the one the suite ran on.
///
/// This is NOT a redundant call. On a nonblocking stream the two SO_*
/// timeouts are silently no-ops, and every read that outruns the bytes already
/// in the receive buffer returns `WouldBlock`, which the parser maps to a
/// dropped connection or a 400 on a perfectly valid request. Restore the
/// blocking contract explicitly instead of assuming the platform did.
/// (`daemon::handle_conn` carries the same workaround, see "BSD-derived
/// kernels" there.)
pub fn prepare_stream(stream: &TcpStream) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));
}

/// A `Read` adapter that fails once a whole-request deadline passes. Checking a
/// fixed deadline on each read turns a steady drip into a bounded one: the next
/// read after the deadline returns `TimedOut`, which the parser treats as a
/// dropped connection.
pub struct DeadlineReader<R> {
    inner: R,
    deadline: Instant,
}

impl<R> DeadlineReader<R> {
    /// Wrap `inner`; reads fail once `budget` has elapsed from now.
    pub fn new(inner: R, budget: Duration) -> Self {
        DeadlineReader {
            inner,
            deadline: Instant::now() + budget,
        }
    }
}

impl<R: Read> Read for DeadlineReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if Instant::now() >= self.deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "request deadline exceeded",
            ));
        }
        self.inner.read(buf)
    }
}

/// A parsed HTTP request.
#[derive(Debug, PartialEq, Eq)]
pub struct HttpRequest {
    /// The method as sent (`GET`, `POST`, …).
    pub method: String,
    /// The request-target, query string included.
    pub path: String,
    /// The declared content type (lowercased, parameters like `; charset=…`
    /// stripped).
    pub content_type: Option<String>,
    /// Every header as `(name, value)`, in order, values trimmed.
    pub headers: Vec<(String, String)>,
    /// The raw body (empty for a request that declared none).
    pub body: Vec<u8>,
}

impl HttpRequest {
    /// The first header called `name`, matched case-insensitively.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Why a request could not be turned into an [`HttpRequest`]. Each maps to a
/// status code; `Io` means the transport failed and there is nothing to answer.
#[derive(Debug)]
pub enum HttpError {
    /// 400 — unparseable request line/headers, or a short body.
    BadRequest,
    /// 405 — parsed, but the method is not one the caller allows.
    MethodNotAllowed,
    /// 413 — declared `Content-Length` exceeds the cap.
    PayloadTooLarge,
    /// The socket closed/timed out mid-request; drop without responding.
    Io,
}

/// Read one HTTP/1.1 request: the request line, the header block up to the blank
/// line, then exactly `Content-Length` body bytes. Bounded in both the header
/// block ([`MAX_HEADER_BYTES`]) and the body (`max_body`) so no single request
/// can exhaust memory. A method outside `allow` is refused as soon as the
/// headers are read, before any body. A `GET` or `HEAD` may omit
/// `Content-Length` (an empty body); every other method must declare one.
/// Reader-based so a test drives it with an in-memory buffer.
pub fn read_http_request<R: BufRead>(
    reader: &mut R,
    max_body: usize,
    allow: &[&str],
) -> Result<HttpRequest, HttpError> {
    let mut header_bytes = 0usize;

    // Request line: exactly three whitespace-separated tokens (METHOD TARGET VER).
    let mut request_line = String::new();
    read_line_capped(reader, &mut request_line, &mut header_bytes)?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next().ok_or(HttpError::BadRequest)?.to_string();
    let path = parts.next().ok_or(HttpError::BadRequest)?.to_string();
    // Require the HTTP-version token so a bare "POST" line is rejected as malformed.
    parts.next().ok_or(HttpError::BadRequest)?;

    let mut content_length: Option<usize> = None;
    let mut content_type: Option<String> = None;
    let mut headers: Vec<(String, String)> = Vec::new();
    loop {
        let mut line = String::new();
        read_line_capped(reader, &mut line, &mut header_bytes)?;
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            let (name, value) = (name.trim(), value.trim());
            if name.eq_ignore_ascii_case("content-length") {
                let n: usize = value.parse().map_err(|_| HttpError::BadRequest)?;
                content_length = Some(n);
            } else if name.eq_ignore_ascii_case("content-type") {
                // Strip a `; charset=…`-style parameter and normalize case, so
                // `application/x-protobuf; charset=utf-8` still matches.
                let base = value.split(';').next().unwrap_or(value).trim();
                content_type = Some(base.to_ascii_lowercase());
            }
            headers.push((name.to_string(), value.to_string()));
        }
        // A header line with no colon is tolerated (skipped), not fatal.
    }

    // Method last: a well-formed disallowed method is a clean 405, not a 400.
    if !allow.iter().any(|m| m.eq_ignore_ascii_case(&method)) {
        return Err(HttpError::MethodNotAllowed);
    }

    let len = match content_length {
        Some(n) => n,
        None if method.eq_ignore_ascii_case("GET") || method.eq_ignore_ascii_case("HEAD") => 0,
        None => return Err(HttpError::BadRequest),
    };
    if len > max_body {
        return Err(HttpError::PayloadTooLarge);
    }
    let mut body = vec![0u8; len];
    reader
        .read_exact(&mut body)
        // A client that declared more than it sent is a bad request, not an
        // internal fault.
        .map_err(|_| HttpError::BadRequest)?;

    Ok(HttpRequest {
        method,
        path,
        content_type,
        headers,
        body,
    })
}

/// Read one line into `out`, charging its bytes against the header budget.
/// Empty read is EOF (the peer closed); overrunning the budget is a bad request.
///
/// The read is bounded to the remaining header budget (`+1`, so a line that would
/// overrun is detected rather than truncated). This matters: `BufRead::read_line`
/// appends the ENTIRE line to `out` before returning, so without the bound a peer
/// streaming a newline-less line could grow `out` without limit — the budget
/// check below would only fire *after* the whole line was already in memory. The
/// `Take` makes the allocation itself bounded by [`MAX_HEADER_BYTES`].
fn read_line_capped<R: BufRead>(
    reader: &mut R,
    out: &mut String,
    total: &mut usize,
) -> Result<(), HttpError> {
    out.clear();
    let remaining = MAX_HEADER_BYTES.saturating_sub(*total);
    let n = reader
        .by_ref()
        .take(remaining as u64 + 1)
        .read_line(out)
        .map_err(|_| HttpError::Io)?;
    if n == 0 {
        return Err(HttpError::Io);
    }
    *total += n;
    if *total > MAX_HEADER_BYTES {
        return Err(HttpError::BadRequest);
    }
    Ok(())
}

/// The reason phrase for the statuses the two listeners send.
fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        302 => "Found",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        415 => "Unsupported Media Type",
        503 => "Service Unavailable",
        _ => "Error",
    }
}

/// The head of an HTTP/1.1 response: status line, `headers`, and a terminating
/// blank line. `Connection: close` keeps the hand-rolled reader single-shot per
/// socket. A caller streaming a body (server-sent events) writes this and then
/// the stream itself; [`write_response`] adds a `Content-Length` and a body.
pub fn response_head(status: u16, headers: &[(&str, &str)]) -> String {
    let mut head = format!("HTTP/1.1 {status} {}\r\n", reason(status));
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("Connection: close\r\n\r\n");
    head
}

/// Write a complete HTTP/1.1 response and flush. Best-effort: a write error
/// means the peer already left.
pub fn write_response(mut out: impl Write, status: u16, headers: &[(&str, &str)], body: &[u8]) {
    let length = body.len().to_string();
    let mut all: Vec<(&str, &str)> = headers.to_vec();
    all.push(("Content-Length", &length));
    let mut bytes = response_head(status, &all).into_bytes();
    bytes.extend_from_slice(body);
    let _ = out.write_all(&bytes);
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    const MAX_BODY: usize = 1 << 20;

    fn parse(bytes: &[u8], max_body: usize) -> Result<HttpRequest, HttpError> {
        read_http_request(&mut Cursor::new(bytes.to_vec()), max_body, &["POST"])
    }

    #[test]
    fn valid_post_yields_method_path_and_body() {
        let raw = b"POST /v1/logs HTTP/1.1\r\nHost: x\r\nContent-Length: 5\r\n\r\nhello";
        let req = parse(raw, MAX_BODY).expect("well-formed POST parses");
        assert_eq!(req.method, "POST");
        assert_eq!(req.path, "/v1/logs");
        assert_eq!(req.body, b"hello");
    }

    #[test]
    fn headers_are_kept_and_looked_up_case_insensitively() {
        let raw = b"POST / HTTP/1.1\r\nHost: 127.0.0.1:1\r\nORIGIN: http://x\r\nContent-Length: 0\r\n\r\n";
        let req = parse(raw, MAX_BODY).unwrap();
        assert_eq!(req.header("origin"), Some("http://x"));
        assert_eq!(req.header("host"), Some("127.0.0.1:1"));
        assert_eq!(req.header("cookie"), None);
    }

    #[test]
    fn oversized_body_is_rejected_before_reading_it() {
        // Declares far more than the (tiny) cap: refused on the header alone,
        // without allocating or reading the body.
        let raw = b"POST /v1/logs HTTP/1.1\r\nContent-Length: 100000\r\n\r\n";
        let err = parse(raw, 16).expect_err("over the cap");
        assert!(matches!(err, HttpError::PayloadTooLarge), "{err:?}");
    }

    #[test]
    fn malformed_request_line_is_a_bad_request() {
        // A single-token request line has no method/target/version split.
        let raw = b"GARBAGE\r\n\r\n";
        let err = parse(raw, MAX_BODY).expect_err("malformed");
        assert!(matches!(err, HttpError::BadRequest), "{err:?}");
    }

    #[test]
    fn a_disallowed_method_is_405_not_400() {
        let raw = b"GET /v1/logs HTTP/1.1\r\nContent-Length: 0\r\n\r\n";
        let err = parse(raw, MAX_BODY).expect_err("GET is not in the allow list");
        assert!(matches!(err, HttpError::MethodNotAllowed), "{err:?}");
    }

    #[test]
    fn a_get_may_omit_content_length_but_a_post_may_not() {
        let get = b"GET / HTTP/1.1\r\nHost: x\r\n\r\n";
        let req = read_http_request(&mut Cursor::new(get.to_vec()), MAX_BODY, &["GET", "POST"])
            .expect("a bodiless GET parses");
        assert!(req.body.is_empty());

        let post = b"POST / HTTP/1.1\r\nHost: x\r\n\r\n";
        let err = read_http_request(&mut Cursor::new(post.to_vec()), MAX_BODY, &["GET", "POST"])
            .expect_err("a POST must declare its length");
        assert!(matches!(err, HttpError::BadRequest), "{err:?}");
    }

    #[test]
    fn an_unterminated_header_line_is_capped_not_buffered_unbounded() {
        // A header value with no CRLF terminator, far larger than the header cap.
        // `read_line` would otherwise append the whole line before any size check;
        // the `Take` bound must refuse it on the budget instead.
        let mut raw = b"POST /v1/logs HTTP/1.1\r\nX: ".to_vec();
        raw.extend(std::iter::repeat_n(b'A', MAX_HEADER_BYTES + 4096));
        let err = parse(&raw, MAX_BODY).expect_err("over the header cap");
        assert!(matches!(err, HttpError::BadRequest), "{err:?}");
    }

    #[test]
    fn deadline_reader_fails_the_read_once_the_budget_is_spent() {
        // A zero budget means the deadline equals construction time; the monotonic
        // clock has advanced by the time `read` runs, so the first read fails
        // rather than dribbling forever (the slowloris defense).
        let data = b"hello world";
        let mut reader = DeadlineReader::new(&data[..], Duration::from_millis(0));
        let mut buf = [0u8; 4];
        let err = reader.read(&mut buf).expect_err("past deadline fails");
        assert_eq!(err.kind(), io::ErrorKind::TimedOut, "{err:?}");
    }

    #[test]
    fn a_short_body_is_a_bad_request_not_a_panic() {
        // Content-Length promises 10 bytes; only 3 are sent.
        let raw = b"POST /v1/logs HTTP/1.1\r\nContent-Length: 10\r\n\r\nabc";
        let err = parse(raw, MAX_BODY).expect_err("body underrun");
        assert!(matches!(err, HttpError::BadRequest), "{err:?}");
    }

    #[test]
    fn a_response_carries_its_headers_a_length_and_close() {
        let mut out = Vec::new();
        write_response(&mut out, 403, &[("X-A", "b")], b"no");
        let text = String::from_utf8(out).unwrap();
        assert!(text.starts_with("HTTP/1.1 403 Forbidden\r\n"), "{text}");
        assert!(text.contains("X-A: b\r\n"), "{text}");
        assert!(text.contains("Content-Length: 2\r\n"), "{text}");
        assert!(text.contains("Connection: close\r\n\r\nno"), "{text}");
    }
}
