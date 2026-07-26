//! Opt-in local OTLP/HTTP receiver (backlog #18, docs/research/token-accounting.md).
//!
//! The three big coding CLIs (Claude Code, Gemini CLI, Codex) can be pointed at
//! an OpenTelemetry endpoint to emit per-request token telemetry with timestamps
//! — higher fidelity than parsing their transcript files after the fact. This
//! module is that endpoint: a `std::net::TcpListener` on `127.0.0.1:<port>`,
//! supervised by the daemon exactly like the attribution thread, that ingests
//! OTLP/HTTP **JSON** exports and buffers the samples for the attribution engine
//! (#17) to prefer over log-parsing.
//!
//! ## Deliberately runtime-free (DESIGN §2)
//! OTLP-over-gRPC would drag in tonic + tokio, contradicting the daemon's
//! tokio-free design. OTLP-over-HTTP with a JSON payload needs neither: this is
//! a hand-rolled, minimal HTTP/1.1 POST reader (request line, headers,
//! `Content-Length` body) over a blocking `TcpStream`, capped like the daemon's
//! frame reader so a client can never make it buffer unbounded input. No `http`
//! crate, no async.
//!
//! ## Version tolerance is the prime directive
//! Every tool's OTLP schema is an undocumented internal that changes without
//! notice, and each uses its own attribute namespace (`claude_code.*`,
//! `gemini_cli.*`, `codex.*`) — there is no universal shape. So parsing is
//! best-effort: an unknown record is skipped, a missing field defaults to 0, and
//! a body that is not valid JSON is a `400` — never a panic and never a crashed
//! thread.
//!
//! ## Scope (what this does and does NOT build)
//! Implemented: the `/v1/logs` OTLP/HTTP+JSON path, which carries the full
//! per-request token breakdown for all three tools. The `/v1/metrics` path is
//! *accepted* (a `200` so an exporter configured for both does not retry-storm)
//! but not parsed — the metric form (`claude_code.token.usage` &c.) splits one
//! request across four single-valued points and adds nothing the log events do
//! not already carry. That deliberate cut is noted in the task hand-off.
//!
//! ## Pointing the tools at the receiver (opt-in, both sides)
//! Enable the receiver first: `[otlp] enabled = true` in tasqx's `config.toml`
//! (optional `[otlp] port`, default 4318). Then configure each tool to export
//! OTLP/HTTP **JSON** logs to `http://127.0.0.1:4318` — the tasqx receiver only
//! reads the `/v1/logs` path, so the endpoint must be the base URL (the exporter
//! appends `/v1/logs` itself):
//!
//! * **Claude Code** — env:
//!   `CLAUDE_CODE_ENABLE_TELEMETRY=1`, `OTEL_LOGS_EXPORTER=otlp`,
//!   `OTEL_EXPORTER_OTLP_PROTOCOL=http/json`,
//!   `OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318`.
//! * **Gemini CLI** — enable telemetry with an OTLP target and the same
//!   `OTEL_EXPORTER_OTLP_PROTOCOL=http/json` +
//!   `OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318` (its `telemetry.outfile`
//!   and an OTLP endpoint are mutually exclusive per session).
//! * **Codex** — the `[otel]` table in `~/.codex/config.toml`: an `otlp-http`
//!   exporter with `protocol = "json"` and
//!   `endpoint = "http://127.0.0.1:4318"`.
//!
//! Because both the receiver and each tool's exporter are off by default, no
//! telemetry leaves the machine unless the user turns both on.
//!
//! ## Trust model
//! The receiver binds `127.0.0.1` only (never `0.0.0.0`), so nothing off the
//! machine can reach it. There is no authentication: any *local* process can
//! POST a forged export and inject arbitrary token counts, which would land as
//! `source=otel` measurements. This is an accepted trade-off — a local attacker
//! already runs as the user — but it is the reason the receiver is opt-in and
//! localhost-bound, and why `otel` measurements are only as trustworthy as the
//! processes on the machine. Do not expose the port beyond loopback.

use std::io::{self, BufRead, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::engine::Engine;
use crate::tokens::UsageSample;

/// Hard cap on an OTLP request body, mirroring `daemon::MAX_FRAME_BYTES`: a
/// client that declares a huge `Content-Length` (or streams forever) must never
/// make the receiver allocate without bound. Real OTLP exports are a few KiB.
const MAX_BODY_BYTES: usize = 1 << 20;

/// Cap on the header block (request line + headers). Well past any real exporter,
/// small enough that a client dribbling headers cannot grow memory unbounded.
const MAX_HEADER_BYTES: usize = 64 * 1024;

/// Per-read/write socket timeout. OTLP posts are small and local; a peer that
/// goes fully idle mid-request must not pin the single receiver thread. This is a
/// PER-READ timeout — it resets on every byte received — so on its own it only
/// catches a *fully* idle peer, not a slow-drip one; [`REQUEST_DEADLINE`] bounds
/// the latter.
const IO_TIMEOUT: Duration = Duration::from_secs(5);

/// Whole-request deadline from the moment a connection is accepted. Unlike
/// [`IO_TIMEOUT`] (which resets on every byte and so is defeated by a peer
/// dribbling one byte just inside the timeout — a slowloris), this is a hard
/// ceiling on the total time one peer may hold the single receiver thread,
/// regardless of how it paces its bytes. Generous for a legitimate local export
/// (a few KiB, sub-millisecond over loopback) yet fatal to a hostile drip.
const REQUEST_DEADLINE: Duration = Duration::from_secs(10);

/// Nonblocking-accept step, matching the attribution/reminder loops' 50 ms
/// shutdown-responsiveness discipline.
const ACCEPT_STEP: Duration = Duration::from_millis(50);

/// The OTLP/HTTP success body. An empty `partialSuccess` means "accepted, nothing
/// rejected" — the response every OTLP exporter expects on a 200.
const PARTIAL_SUCCESS: &str = r#"{"partialSuccess":{}}"#;

/// One raw per-request sample lifted from an OTLP export, before any task
/// attribution: the four-way [`UsageSample`] plus the correlation facts the
/// buffer needs to match it to a task later (`session_id`) and the tool that
/// emitted it. The engine writes these straight into `otlp_samples`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtlpSample {
    /// Which tool emitted the record (`claude-code`, `gemini-cli`, `codex`), as
    /// `tool_of` resolved it from the event's attribute namespace. Captured here
    /// because that namespace is the only place an exporter names itself, and it
    /// becomes the stored measurement's `tool` when the completing agent supplied
    /// no `client` of its own.
    pub tool: String,
    /// The correlating session/conversation id, from whichever of `SESSION_KEYS`
    /// this tool spells it with. `None` when the record carried none: such a
    /// sample is still buffered but can never be matched to a task — attribution
    /// looks the buffer up by session id alone — so it simply ages out under the
    /// retention prune.
    pub session_id: Option<String>,
    /// The timestamped four-way counts, already mapped onto tasqx's schema by the
    /// per-tool rules in `sample_from_record` (Codex's cached subset subtracted
    /// out of input, Gemini's thought tokens folded into output). That mapping
    /// mirrors the transcript parsers exactly, so a telemetry sample and a
    /// log-parsed one are directly comparable — which is what lets attribution
    /// choose either source for a task without changing what the numbers mean.
    pub sample: UsageSample,
}

// ---- receiver thread --------------------------------------------------------

/// Bind `127.0.0.1:<port>` and serve OTLP/HTTP until `shutdown` is set. Spawned
/// by `daemon::serve_with_options` only when `[otlp] enabled` (opt-in, off by
/// default). Blocking; runs on its own supervised thread.
///
/// A bind failure (port already in use) is logged and the thread simply exits —
/// the OTLP receiver is an auxiliary, opt-in feature and must never take the rest
/// of the daemon down with it (unlike the store-backed attribution/reminder
/// threads, whose failures are genuinely fatal).
pub fn run_receiver(engine: Arc<Mutex<Engine>>, port: u16, shutdown: Arc<AtomicBool>) {
    let listener = match TcpListener::bind((Ipv4Addr::LOCALHOST, port)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("tasqx daemon: OTLP receiver disabled — cannot bind 127.0.0.1:{port}: {e}");
            return;
        }
    };
    if let Err(e) = listener.set_nonblocking(true) {
        eprintln!("tasqx daemon: OTLP receiver disabled — {e}");
        return;
    }
    eprintln!("tasqx daemon: OTLP receiver on http://127.0.0.1:{port}/v1/logs");
    accept_loop(&listener, &engine, &shutdown);
}

/// The nonblocking accept loop, split from [`run_receiver`] so a test can drive
/// it against an ephemeral, already-bound listener. `listener` must already be
/// set nonblocking. Returns when `shutdown` is set.
fn accept_loop(listener: &TcpListener, engine: &Arc<Mutex<Engine>>, shutdown: &Arc<AtomicBool>) {
    while !shutdown.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _addr)) => handle_connection(stream, engine),
            // No pending connection: sleep one step and re-check shutdown.
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => thread::sleep(ACCEPT_STEP),
            // A transient accept error must not spin the thread hot; step and retry.
            Err(_) => thread::sleep(ACCEPT_STEP),
        }
    }
}

/// Handle one connection: read the request, ingest any samples, write the
/// response. Blocking with a deadline; any transport error just drops the peer.
fn handle_connection(stream: TcpStream, engine: &Arc<Mutex<Engine>>) {
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));

    // `&TcpStream` implements both Read and Write, so the reader borrows the
    // stream immutably and the response write below takes a second shared borrow.
    // The read side is wrapped in a `DeadlineReader` so the whole request is
    // bounded in wall-clock time (defeating a slow-drip peer the per-read
    // `IO_TIMEOUT` alone cannot catch).
    let (status, body) = {
        let mut reader = io::BufReader::new(DeadlineReader::new(&stream, REQUEST_DEADLINE));
        match read_http_request(&mut reader, MAX_BODY_BYTES) {
            Ok(req) => dispatch(&req, engine),
            Err(HttpError::MethodNotAllowed) => (405, "method not allowed"),
            Err(HttpError::PayloadTooLarge) => (413, "payload too large"),
            Err(HttpError::BadRequest) => (400, "bad request"),
            // The peer closed or timed out mid-request: nothing to answer.
            Err(HttpError::Io) => return,
        }
    };
    write_response(&stream, status, body);
}

/// Route one parsed request. Only POST reaches here (non-POST is rejected in
/// [`read_http_request`]). `/v1/logs` and `/v1/metrics` are accepted; only logs
/// are parsed (see module docs). Any other path is `404`.
fn dispatch(req: &HttpRequest, engine: &Arc<Mutex<Engine>>) -> (u16, &'static str) {
    // Strip a query string; OTLP exporters do not use one, but be lenient.
    let path = req.path.split(['?', '#']).next().unwrap_or(&req.path);
    if path != "/v1/logs" && path != "/v1/metrics" {
        return (404, "not found");
    }
    // A body that is not valid JSON is a genuine client error: 400. A body that
    // is valid JSON but an unknown shape yields zero samples and still succeeds
    // (version tolerance) — an old/new tool schema must not read as a failure.
    let Ok(root) = serde_json::from_slice::<Value>(&req.body) else {
        return (400, "invalid json");
    };
    let samples = samples_from_otlp_logs(&root);
    if !samples.is_empty() {
        let guard = engine.lock().unwrap_or_else(|p| p.into_inner());
        if let Err(e) = guard.otlp_ingest(&samples) {
            // A store fault here must not fail the export (the exporter would
            // retry the same batch forever); log and still answer 200.
            eprintln!("tasqx daemon: OTLP ingest failed: {}", e.message);
        }
    }
    (200, PARTIAL_SUCCESS)
}

/// Write a minimal HTTP/1.1 response and close. Best-effort: a write error means
/// the peer already left.
fn write_response(mut stream: &TcpStream, status: u16, body: &str) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        413 => "Payload Too Large",
        _ => "Error",
    };
    // `Connection: close` keeps the hand-rolled reader single-shot per socket;
    // `Allow: POST` is required on a 405 and harmless elsewhere.
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: application/json\r\n\
         Content-Length: {}\r\n\
         Allow: POST\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
}

/// A `Read` adapter that fails once a whole-request deadline passes. The socket's
/// [`IO_TIMEOUT`] is a *per-read* timeout: it resets on every byte, so a peer
/// dribbling one byte just inside it makes progress forever while holding the
/// single receiver thread (slowloris). Checking a fixed deadline on each read
/// turns that steady drip into a bounded one — the next read after the deadline
/// returns `TimedOut`, which the parser treats as a dropped connection.
struct DeadlineReader<R> {
    inner: R,
    deadline: Instant,
}

impl<R> DeadlineReader<R> {
    fn new(inner: R, budget: Duration) -> Self {
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
                "OTLP request deadline exceeded",
            ));
        }
        self.inner.read(buf)
    }
}

// ---- HTTP/1.1 request parsing (hand-rolled, no `http` crate) -----------------

/// A parsed HTTP request: method, request-target, and the raw body.
#[derive(Debug, PartialEq, Eq)]
struct HttpRequest {
    method: String,
    path: String,
    body: Vec<u8>,
}

/// Why a request could not be turned into an [`HttpRequest`]. Each maps to a
/// status code; `Io` means the transport failed and there is nothing to answer.
#[derive(Debug)]
enum HttpError {
    /// 400 — unparseable request line/headers, or a short body.
    BadRequest,
    /// 405 — parsed, but the method is not POST.
    MethodNotAllowed,
    /// 413 — declared `Content-Length` exceeds the cap.
    PayloadTooLarge,
    /// The socket closed/timed out mid-request; drop without responding (the
    /// specific transport error is not worth surfacing for a normal disconnect).
    Io,
}

/// Read one HTTP/1.1 request: the request line, the header block up to the blank
/// line, then exactly `Content-Length` body bytes. Bounded in both the header
/// block ([`MAX_HEADER_BYTES`]) and the body (`max_body`) so no single request
/// can exhaust memory. Reader-based so a test drives it with an in-memory buffer.
fn read_http_request<R: BufRead>(
    reader: &mut R,
    max_body: usize,
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

    // Headers until the blank line; we only care about Content-Length.
    let mut content_length: Option<usize> = None;
    loop {
        let mut line = String::new();
        read_line_capped(reader, &mut line, &mut header_bytes)?;
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            if name.trim().eq_ignore_ascii_case("content-length") {
                let n: usize = value.trim().parse().map_err(|_| HttpError::BadRequest)?;
                content_length = Some(n);
            }
        }
        // A header line with no colon is tolerated (skipped), not fatal.
    }

    // Method last: a well-formed non-POST is a clean 405, not a 400.
    if !method.eq_ignore_ascii_case("POST") {
        return Err(HttpError::MethodNotAllowed);
    }

    let len = content_length.ok_or(HttpError::BadRequest)?;
    if len > max_body {
        return Err(HttpError::PayloadTooLarge);
    }
    let mut body = vec![0u8; len];
    reader
        .read_exact(&mut body)
        // A client that declared more than it sent is a bad request, not an
        // internal fault.
        .map_err(|_| HttpError::BadRequest)?;

    Ok(HttpRequest { method, path, body })
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

// ---- OTLP/HTTP JSON -> UsageSample (pure, per-tool) -------------------------

/// The tool label stored for an OTLP export, keyed off its attribute namespace.
/// Returns the human-facing tool string and a flag for the Codex cache subtree
/// (its `cached_input_tokens` is a subset of `input_tokens`). `None` for a
/// namespace tasqx does not recognize — that record is skipped.
fn tool_of(event: &str) -> Option<&'static str> {
    if event.starts_with("claude_code") {
        Some("claude-code")
    } else if event.starts_with("gemini_cli") {
        Some("gemini-cli")
    } else if event.starts_with("codex") {
        Some("codex")
    } else {
        None
    }
}

/// Extract every recognizable per-request sample from an OTLP/HTTP `LogsData`
/// JSON document. Walks `resourceLogs[].scopeLogs[].logRecords[]`, tolerating
/// missing levels. Unknown tools, records with no usable timestamp, and
/// zero-token records are skipped; nothing here can panic on hostile input.
pub fn samples_from_otlp_logs(root: &Value) -> Vec<OtlpSample> {
    let mut out = Vec::new();
    let resource_logs = root.get("resourceLogs").and_then(Value::as_array);
    for rl in resource_logs.into_iter().flatten() {
        let scope_logs = rl.get("scopeLogs").and_then(Value::as_array);
        for sl in scope_logs.into_iter().flatten() {
            let records = sl.get("logRecords").and_then(Value::as_array);
            for record in records.into_iter().flatten() {
                if let Some(sample) = sample_from_record(record) {
                    out.push(sample);
                }
            }
        }
    }
    out
}

/// Map one OTLP log record onto an [`OtlpSample`], or `None` when it is not a
/// recognizable, non-empty token event.
fn sample_from_record(record: &Value) -> Option<OtlpSample> {
    let attrs = record.get("attributes").and_then(Value::as_array);

    // The event name lives in the record body for Claude Code / Codex, and in an
    // `event.name` attribute for Gemini CLI — accept either.
    let event = record
        .get("body")
        .and_then(|b| b.get("stringValue"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| attr_str(attrs, "event.name"))?;
    let tool = tool_of(&event)?;

    // Timestamp: nanoseconds since the epoch, as a JSON string or number. Fall
    // back to the observed time. A record with no usable time is unbucketable.
    let ts_nanos = read_unix_nanos(record, "timeUnixNano")
        .or_else(|| read_unix_nanos(record, "observedTimeUnixNano"))?;
    let ts = nanos_to_rfc3339(ts_nanos)?;

    let (input, output, cache_read, cache_creation) = match tool {
        "codex" => {
            // Codex: `cached_input_tokens` is a *subset* of `input_tokens`, and
            // there is no cache-creation concept — mirror the file parser so the
            // four-field schema stays comparable across tools.
            let raw_input = attr_u64(attrs, "input_tokens");
            let cached = attr_u64(attrs, "cached_input_tokens");
            (
                raw_input.saturating_sub(cached),
                attr_u64(attrs, "output_tokens"),
                cached,
                0,
            )
        }
        "gemini-cli" => {
            // Gemini: thought tokens are billed as output; cached taken as-is
            // (not subtracted); no cache-creation counter. Mirrors the file parser.
            let output = attr_u64(attrs, "output_token_count")
                .saturating_add(attr_u64(attrs, "thoughts_token_count"));
            (
                attr_u64(attrs, "input_token_count"),
                output,
                attr_u64(attrs, "cached_content_token_count"),
                0,
            )
        }
        _ => {
            // Claude Code: four disjoint counters, taken as-is (its cache fields
            // are separate from input, exactly like its transcript usage block).
            (
                attr_u64(attrs, "input_tokens"),
                attr_u64(attrs, "output_tokens"),
                attr_u64(attrs, "cache_read_tokens"),
                attr_u64(attrs, "cache_creation_tokens"),
            )
        }
    };

    // A record with no tokens at all is noise; do not buffer it.
    if input == 0 && output == 0 && cache_read == 0 && cache_creation == 0 {
        return None;
    }

    let session_id = SESSION_KEYS.iter().find_map(|k| attr_str(attrs, k));
    let model = MODEL_KEYS.iter().find_map(|k| attr_str(attrs, k));

    Some(OtlpSample {
        tool: tool.to_string(),
        session_id,
        sample: UsageSample {
            ts,
            model,
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: cache_read,
            cache_creation_tokens: cache_creation,
        },
    })
}

/// Attribute keys that carry a task-correlating session/conversation id, tried
/// in order. Each tool spells it differently (`session.id`, Codex's
/// `conversation.id`), so the receiver accepts the union rather than one shape.
const SESSION_KEYS: [&str; 4] = [
    "session.id",
    "session_id",
    "conversation.id",
    "conversation_id",
];

/// Attribute keys that name the model, tried in order.
const MODEL_KEYS: [&str; 2] = ["model", "gen_ai.request.model"];

/// Look up an OTLP attribute (an array of `{"key","value"}` objects) and read it
/// as a `u64`. Values arrive as `intValue` (an int64, JSON-encoded as a *string*
/// per proto3, but some emitters send a number), or occasionally `doubleValue`
/// or a numeric `stringValue`. Absent or unreadable => 0.
fn attr_u64(attrs: Option<&Vec<Value>>, key: &str) -> u64 {
    let Some(value) = attr_value(attrs, key) else {
        return 0;
    };
    if let Some(n) = value.get("intValue") {
        return n
            .as_u64()
            .or_else(|| n.as_str().and_then(|s| s.parse().ok()))
            .unwrap_or(0);
    }
    if let Some(d) = value.get("doubleValue").and_then(Value::as_f64) {
        if d.is_finite() && d >= 0.0 {
            return d as u64;
        }
    }
    value
        .get("stringValue")
        .and_then(Value::as_str)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// Look up an OTLP attribute and read it as a non-empty string (`stringValue`).
fn attr_str(attrs: Option<&Vec<Value>>, key: &str) -> Option<String> {
    attr_value(attrs, key)?
        .get("stringValue")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The `value` object of the attribute named `key`, if present.
fn attr_value<'a>(attrs: Option<&'a Vec<Value>>, key: &str) -> Option<&'a Value> {
    attrs?.iter().find_map(|kv| {
        (kv.get("key").and_then(Value::as_str) == Some(key)).then(|| kv.get("value"))?
    })
}

/// Read a `*UnixNano` field (JSON string or number) as nanoseconds.
fn read_unix_nanos(record: &Value, key: &str) -> Option<i128> {
    let v = record.get(key)?;
    if let Some(s) = v.as_str() {
        return s.parse::<i128>().ok();
    }
    v.as_u64()
        .map(i128::from)
        .or_else(|| v.as_i64().map(i128::from))
}

/// Convert epoch nanoseconds to a canonical RFC3339 string, so OTLP sample
/// timestamps compare byte-for-byte with the transcript parsers' (which also go
/// through jiff). Out-of-range values are skipped rather than clamped.
fn nanos_to_rfc3339(nanos: i128) -> Option<String> {
    jiff::Timestamp::from_nanosecond(nanos)
        .ok()
        .map(|t| t.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    // ---- HTTP frame parser ----

    fn parse(bytes: &[u8], max_body: usize) -> Result<HttpRequest, HttpError> {
        read_http_request(&mut Cursor::new(bytes.to_vec()), max_body)
    }

    #[test]
    fn valid_post_yields_method_path_and_body() {
        let raw = b"POST /v1/logs HTTP/1.1\r\nHost: x\r\nContent-Length: 5\r\n\r\nhello";
        let req = parse(raw, MAX_BODY_BYTES).expect("well-formed POST parses");
        assert_eq!(req.method, "POST");
        assert_eq!(req.path, "/v1/logs");
        assert_eq!(req.body, b"hello");
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
        let err = parse(raw, MAX_BODY_BYTES).expect_err("malformed");
        assert!(matches!(err, HttpError::BadRequest), "{err:?}");
    }

    #[test]
    fn a_non_post_method_is_405_not_400() {
        let raw = b"GET /v1/logs HTTP/1.1\r\nContent-Length: 0\r\n\r\n";
        let err = parse(raw, MAX_BODY_BYTES).expect_err("GET is not accepted");
        assert!(matches!(err, HttpError::MethodNotAllowed), "{err:?}");
    }

    #[test]
    fn an_unterminated_header_line_is_capped_not_buffered_unbounded() {
        // A header value with no CRLF terminator, far larger than the header cap.
        // `read_line` would otherwise append the whole line before any size check;
        // the `Take` bound must refuse it on the budget instead.
        let mut raw = b"POST /v1/logs HTTP/1.1\r\nX: ".to_vec();
        raw.extend(std::iter::repeat_n(b'A', MAX_HEADER_BYTES + 4096));
        let err = parse(&raw, MAX_BODY_BYTES).expect_err("over the header cap");
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
        let err = parse(raw, MAX_BODY_BYTES).expect_err("body underrun");
        assert!(matches!(err, HttpError::BadRequest), "{err:?}");
    }

    // ---- OTLP JSON -> UsageSample, per tool ----

    /// Build a one-record OTLP LogsData document with the given body event name
    /// and attributes (`[(key, AnyValue-json)]`).
    fn logs_doc(event: &str, time_nanos: &str, attrs: &[(&str, &str)]) -> Value {
        let attr_json: Vec<Value> = attrs
            .iter()
            .map(|(k, v)| {
                serde_json::json!({ "key": k, "value": serde_json::from_str::<Value>(v).unwrap() })
            })
            .collect();
        serde_json::json!({
            "resourceLogs": [{
                "scopeLogs": [{
                    "logRecords": [{
                        "timeUnixNano": time_nanos,
                        "body": { "stringValue": event },
                        "attributes": attr_json,
                    }]
                }]
            }]
        })
    }

    #[test]
    fn claude_code_maps_four_disjoint_counters() {
        // 2026-07-24T10:00:00Z == 1_784_887_200 s.
        let doc = logs_doc(
            "claude_code.api_request",
            "1784887200000000000",
            &[
                ("input_tokens", r#"{"intValue":"100"}"#),
                ("output_tokens", r#"{"intValue":"50"}"#),
                ("cache_read_tokens", r#"{"intValue":"30"}"#),
                ("cache_creation_tokens", r#"{"intValue":"7"}"#),
                ("model", r#"{"stringValue":"claude-opus-4-8"}"#),
                ("session.id", r#"{"stringValue":"sess-1"}"#),
            ],
        );
        let s = samples_from_otlp_logs(&doc);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].tool, "claude-code");
        assert_eq!(s[0].session_id.as_deref(), Some("sess-1"));
        assert_eq!(s[0].sample.input_tokens, 100);
        assert_eq!(s[0].sample.output_tokens, 50);
        assert_eq!(s[0].sample.cache_read_tokens, 30);
        assert_eq!(s[0].sample.cache_creation_tokens, 7);
        assert_eq!(s[0].sample.model.as_deref(), Some("claude-opus-4-8"));
        assert_eq!(s[0].sample.ts, "2026-07-24T10:00:00Z");
    }

    #[test]
    fn gemini_folds_thoughts_into_output_and_keeps_cache_as_is() {
        // Event name in an `event.name` attribute (Gemini's placement), no body.
        let doc = serde_json::json!({
            "resourceLogs": [{ "scopeLogs": [{ "logRecords": [{
                "timeUnixNano": "1774339200000000000",
                "attributes": [
                    { "key": "event.name", "value": { "stringValue": "gemini_cli.api_response" } },
                    { "key": "input_token_count", "value": { "intValue": "200" } },
                    { "key": "output_token_count", "value": { "intValue": "40" } },
                    { "key": "thoughts_token_count", "value": { "intValue": "10" } },
                    { "key": "cached_content_token_count", "value": { "intValue": "5" } }
                ]
            }]}]}]
        });
        let s = samples_from_otlp_logs(&doc);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].tool, "gemini-cli");
        assert_eq!(s[0].sample.input_tokens, 200);
        // output = output_token_count + thoughts_token_count.
        assert_eq!(s[0].sample.output_tokens, 50);
        // cached taken as-is, not subtracted from input.
        assert_eq!(s[0].sample.cache_read_tokens, 5);
        assert_eq!(s[0].sample.cache_creation_tokens, 0);
    }

    #[test]
    fn codex_subtracts_cached_from_input_and_has_no_cache_creation() {
        let doc = logs_doc(
            "codex.api_request",
            "1774339200000000000",
            &[
                ("input_tokens", r#"{"intValue":"12811"}"#),
                ("cached_input_tokens", r#"{"intValue":"3456"}"#),
                ("output_tokens", r#"{"intValue":"341"}"#),
                ("conversation.id", r#"{"stringValue":"conv-9"}"#),
            ],
        );
        let s = samples_from_otlp_logs(&doc);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].tool, "codex");
        assert_eq!(s[0].session_id.as_deref(), Some("conv-9"));
        // fresh input = input - cached.
        assert_eq!(s[0].sample.input_tokens, 12811 - 3456);
        assert_eq!(s[0].sample.cache_read_tokens, 3456);
        assert_eq!(s[0].sample.output_tokens, 341);
        assert_eq!(s[0].sample.cache_creation_tokens, 0);
    }

    #[test]
    fn int_values_are_read_from_both_string_and_number_encodings() {
        // proto3 JSON encodes int64 as a string, but some emitters send a bare
        // number — accept both.
        let doc = logs_doc(
            "claude_code.api_request",
            "1774339200000000000",
            &[
                ("input_tokens", r#"{"intValue":123}"#),
                ("output_tokens", r#"{"intValue":"456"}"#),
            ],
        );
        let s = samples_from_otlp_logs(&doc);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].sample.input_tokens, 123);
        assert_eq!(s[0].sample.output_tokens, 456);
    }

    #[test]
    fn an_unknown_tool_namespace_is_skipped() {
        let doc = logs_doc(
            "some_other_agent.api_request",
            "1774339200000000000",
            &[("input_tokens", r#"{"intValue":"10"}"#)],
        );
        assert!(samples_from_otlp_logs(&doc).is_empty());
    }

    #[test]
    fn a_zero_token_record_is_not_buffered() {
        let doc = logs_doc(
            "claude_code.api_request",
            "1774339200000000000",
            &[("input_tokens", r#"{"intValue":"0"}"#)],
        );
        assert!(samples_from_otlp_logs(&doc).is_empty());
    }

    #[test]
    fn a_record_without_a_usable_timestamp_is_skipped() {
        let doc = logs_doc(
            "claude_code.api_request",
            "not-a-number",
            &[("input_tokens", r#"{"intValue":"10"}"#)],
        );
        assert!(samples_from_otlp_logs(&doc).is_empty());
    }

    #[test]
    fn a_totally_unknown_shape_yields_no_samples_rather_than_panicking() {
        // Version tolerance: a document with none of the expected levels.
        let doc = serde_json::json!({ "somethingElse": 42, "resourceLogs": "not-an-array" });
        assert!(samples_from_otlp_logs(&doc).is_empty());
    }

    // ---- end-to-end over a real socket ----

    #[test]
    fn a_posted_export_is_parsed_and_buffered_then_answered_200() {
        use std::io::Read;
        use std::net::TcpStream;

        // Ephemeral port so parallel test runs never collide; the loop is driven
        // directly (not through the bind path) so we know the address to POST to.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind ephemeral");
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();

        let engine = Arc::new(Mutex::new(Engine::open_in_memory().unwrap()));
        let shutdown = Arc::new(AtomicBool::new(false));
        let handle = {
            let engine = engine.clone();
            let shutdown = shutdown.clone();
            thread::spawn(move || accept_loop(&listener, &engine, &shutdown))
        };

        let body = serde_json::to_string(&logs_doc(
            "claude_code.api_request",
            "1774339200000000000",
            &[
                ("input_tokens", r#"{"intValue":"100"}"#),
                ("output_tokens", r#"{"intValue":"50"}"#),
                ("session.id", r#"{"stringValue":"sess-live"}"#),
            ],
        ))
        .unwrap();
        let request = format!(
            "POST /v1/logs HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );

        let mut stream = TcpStream::connect(addr).expect("connect to receiver");
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();

        shutdown.store(true, Ordering::Relaxed);
        handle.join().unwrap();

        assert!(
            response.starts_with("HTTP/1.1 200"),
            "expected 200, got: {response}"
        );
        assert!(response.contains("partialSuccess"), "{response}");

        // The sample landed in the buffer, correlated by its session id.
        let (samples, tool) = engine
            .lock()
            .unwrap()
            .otlp_samples_for_session("sess-live")
            .unwrap();
        assert_eq!(samples.len(), 1, "the POSTed sample was buffered");
        assert_eq!(samples[0].input_tokens, 100);
        assert_eq!(samples[0].output_tokens, 50);
        assert_eq!(tool.as_deref(), Some("claude-code"));
    }
}
