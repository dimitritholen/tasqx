//! The three S3 calls this connector makes, signed with SigV4 and sent over a
//! blocking `ureq` agent: a ranged GET to probe, a GET to download, a
//! conditional PUT to upload.

use std::fs::File;
use std::io::Read;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ureq::http;
use ureq::tls::{RootCerts, TlsConfig, TlsProvider};

use crate::sigv4;

/// Replaces `https://<account_id>.r2.cloudflarestorage.com` when set: for the
/// tests' fake bucket, or an R2 jurisdiction endpoint such as
/// `https://<account_id>.eu.r2.cloudflarestorage.com`. Read on every call.
pub const ENDPOINT_ENV: &str = "TASQX_R2_ENDPOINT";

/// How long to wait for the TCP and TLS handshake.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long one whole call may take, body included: nine minutes, room for a
/// multi-megabyte upload on a slow link. Under the ten-minute kill `tasqx
/// sync` gives a `pull` or `push` (`tasqx_core::remote::TRANSFER_TIMEOUT`,
/// D201), so a stalled transfer ends here with a reason rather than there
/// with a signal. `describe` and `configure` never get here with a body, and
/// their probe answers in seconds.
const CALL_TIMEOUT: Duration = Duration::from_secs(540);

/// R2 takes any region; `auto` is the one its docs sign with.
const REGION: &str = "auto";

/// The SHA-256 of an empty payload.
const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// Where requests go: `base` is `scheme://authority`, `host` the authority.
#[derive(Debug, Clone)]
pub struct Endpoint {
    base: String,
    host: String,
}

impl Endpoint {
    /// The account's own S3 endpoint.
    pub fn r2(account_id: &str) -> Self {
        let host = format!("{account_id}.r2.cloudflarestorage.com");
        Self {
            base: format!("https://{host}"),
            host,
        }
    }

    /// An override, `https://host[:port]` with no path. Plain `http://` only
    /// for a loopback host: anywhere else it would carry the signed requests
    /// and the snapshot in clear.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let base = raw.trim().trim_end_matches('/');
        let (host, tls) = match base.strip_prefix("https://") {
            Some(h) => (h, true),
            None => (base.strip_prefix("http://").unwrap_or_default(), false),
        };
        if host.is_empty() || host.contains(['/', '?', '#', '@']) {
            return Err(format!(
                "{ENDPOINT_ENV}={raw:?} is not an https://host[:port] URL"
            ));
        }
        if !tls && !is_loopback(host) {
            return Err(format!(
                "{ENDPOINT_ENV}={raw:?} is plain http to a host that is not this machine; \
                 use https://, since the request would carry the snapshot and its \
                 signature unencrypted"
            ));
        }
        Ok(Self {
            host: host.to_string(),
            base: base.to_string(),
        })
    }

    /// The account endpoint, or the override from the environment.
    pub fn resolve(account_id: &str) -> Result<Self, String> {
        match std::env::var(ENDPOINT_ENV) {
            Ok(raw) if !raw.trim().is_empty() => Self::parse(&raw),
            _ => Ok(Self::r2(account_id)),
        }
    }
}

/// Whether `authority` (`host[:port]`) names this machine: `localhost`,
/// 127.0.0.0/8, or `[::1]`.
fn is_loopback(authority: &str) -> bool {
    let host = match authority.strip_prefix('[') {
        Some(rest) => match rest.split_once(']') {
            Some((ip, "")) => ip,
            Some((ip, port)) if port.starts_with(':') => ip,
            _ => return false,
        },
        None => authority.split_once(':').map_or(authority, |(h, _)| h),
    };
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// A signed-request client for one bucket's credentials.
pub struct Client {
    agent: ureq::Agent,
    endpoint: Endpoint,
    access_key_id: String,
    secret: String,
}

/// What a PUT carries: the open file and the SHA-256 of its bytes.
pub struct Upload {
    pub file: File,
    pub sha256: String,
}

impl Client {
    pub fn new(endpoint: Endpoint, access_key_id: &str, secret: &str) -> Self {
        let tls = TlsConfig::builder()
            // rustls over ring, so no system OpenSSL is linked; roots from
            // the OS trust store, not the bundled webpki-roots (D199).
            .provider(TlsProvider::Rustls)
            .root_certs(RootCerts::PlatformVerifier)
            .build();
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_global(Some(CALL_TIMEOUT))
            // A signed request is for one host; never replay it at another.
            .max_redirects(0)
            .tls_config(tls)
            .build()
            .into();
        Self {
            agent,
            endpoint,
            access_key_id: access_key_id.to_string(),
            secret: secret.to_string(),
        }
    }

    /// Where requests go, for messages.
    pub fn base(&self) -> &str {
        &self.endpoint.base
    }

    /// Send one signed request for `path` (`/bucket/key`, not yet encoded).
    /// `headers` are extra headers, lowercase, all of them signed. A response
    /// with any status is `Ok`; only a failure to get one is `Err`.
    pub fn send(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        upload: Option<Upload>,
    ) -> Result<http::Response<ureq::Body>, String> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "the system clock is set before 1970".to_string())?
            .as_secs();
        let amz_date = sigv4::amz_date(now);
        let payload = upload
            .as_ref()
            .map_or(EMPTY_SHA256, |u| u.sha256.as_str())
            .to_string();
        let uri = sigv4::uri_encode_path(path);
        let mut signed: Vec<(&str, &str)> = vec![
            // Set by hand, so what is signed is exactly what is sent.
            ("host", &self.endpoint.host),
            ("x-amz-date", &amz_date),
            ("x-amz-content-sha256", &payload),
        ];
        signed.extend_from_slice(headers);
        let auth = sigv4::authorization(
            method,
            &uri,
            &signed,
            &payload,
            &amz_date,
            REGION,
            "s3",
            &self.access_key_id,
            &self.secret,
        );
        let mut builder = http::Request::builder()
            .method(method)
            .uri(format!("{}{uri}", self.endpoint.base));
        for (k, v) in &signed {
            builder = builder.header(*k, *v);
        }
        builder = builder.header("authorization", auth);
        let sent = match upload {
            None => builder
                .body(())
                .map_err(|e| e.to_string())
                .map(|r| self.agent.run(r)),
            Some(u) => builder
                .body(u.file)
                .map_err(|e| e.to_string())
                .map(|r| self.agent.run(r)),
        }
        .map_err(|e| format!("cannot build the request: {e}"))?;
        sent.map_err(|e| match e {
            ureq::Error::Timeout(_) => {
                format!("R2 at {} did not answer in time ({e})", self.base())
            }
            e => format!("cannot reach R2 at {}: {e}", self.base()),
        })
    }
}

/// The response's ETag without quotes (or a weak marker): the version.
pub fn etag(resp: &http::Response<ureq::Body>) -> Option<String> {
    let raw = resp.headers().get("etag")?.to_str().ok()?.trim();
    let bare = raw.strip_prefix("W/").unwrap_or(raw).trim_matches('"');
    (!bare.is_empty() && bare.bytes().all(|b| b.is_ascii_graphic())).then(|| bare.to_string())
}

/// An S3 error response: its status, and the `<Code>` of its XML body.
pub struct Failure {
    pub status: u16,
    pub code: Option<String>,
    pub message: Option<String>,
}

impl Failure {
    /// Read the (small) error body; at most 64 KiB of it.
    pub fn read(resp: http::Response<ureq::Body>) -> Self {
        let status = resp.status().as_u16();
        let mut text = String::new();
        let _ = resp
            .into_body()
            .into_reader()
            .take(64 * 1024)
            .read_to_string(&mut text);
        Self {
            status,
            code: between(&text, "<Code>", "</Code>"),
            message: between(&text, "<Message>", "</Message>"),
        }
    }

    pub fn is(&self, code: &str) -> bool {
        self.code.as_deref() == Some(code)
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HTTP {}", self.status)?;
        if let Some(code) = &self.code {
            write!(f, " {code}")?;
        }
        if let Some(message) = &self.message {
            write!(f, ": {message}")?;
        }
        Ok(())
    }
}

fn between(text: &str, open: &str, close: &str) -> Option<String> {
    let start = text.find(open)? + open.len();
    let len = text[start..].find(close)?;
    Some(text[start..start + len].trim().to_string()).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stalled transfer must end here, with a reason, before `tasqx sync`'s
    /// runner kills the process (D199, D201) — and leave a multi-megabyte
    /// upload on a slow link most of that time rather than 90 s of it.
    #[test]
    fn a_call_times_out_before_the_sync_runner_kills_it() {
        let runner = tasqx_core::remote::TRANSFER_TIMEOUT;
        assert!(CALL_TIMEOUT < runner, "{CALL_TIMEOUT:?} vs {runner:?}");
        assert!(
            CALL_TIMEOUT + CONNECT_TIMEOUT < runner,
            "the handshake too fits under the kill"
        );
        assert!(CALL_TIMEOUT >= Duration::from_secs(540), "{CALL_TIMEOUT:?}");
    }

    #[test]
    fn plain_http_is_accepted_for_a_loopback_host_only() {
        for ok in [
            "http://127.0.0.1:8080",
            "http://127.5.6.7",
            "http://localhost:9000/",
            "http://LOCALHOST",
            "http://[::1]:9000",
        ] {
            assert!(Endpoint::parse(ok).is_ok(), "{ok}");
        }
        for refused in [
            "http://example.com",
            "http://10.0.0.1:9000",
            "http://127.0.0.1.evil.example",
            "http://localhost.evil.example",
            "http://[::2]:9000",
        ] {
            let msg = Endpoint::parse(refused).unwrap_err();
            assert!(msg.contains(ENDPOINT_ENV), "{refused}: {msg}");
            assert!(msg.contains("https://"), "{refused}: {msg}");
        }
    }

    #[test]
    fn https_is_accepted_for_any_host() {
        for ok in [
            "https://example.com",
            "https://0123456789abcdef0123456789abcdef.eu.r2.cloudflarestorage.com",
            "https://10.0.0.1:8443",
        ] {
            let e = Endpoint::parse(ok).unwrap();
            assert_eq!(e.base, ok);
        }
    }

    /// An https endpoint gets as far as a TLS handshake: the provider and the
    /// OS trust store are wired, so the only failure is the peer's.
    #[test]
    fn https_reaches_a_tls_handshake() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        // A peer that accepts, then hangs up without speaking TLS.
        std::thread::spawn(move || {
            for s in listener.incoming() {
                drop(s);
            }
        });
        let endpoint = Endpoint::parse(&format!("https://127.0.0.1:{port}")).unwrap();
        let client = Client::new(endpoint, "AKID", "secret");
        let err = client.send("GET", "/b/k", &[], None).unwrap_err();
        assert!(err.contains("cannot reach R2"), "{err}");
    }
}
