//! AWS Signature Version 4, the narrow part R2's S3 API needs: a header-signed
//! request with no query string.

use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};

/// Sign one request and return the `Authorization` header value.
///
/// `headers` are the headers to sign, names lowercase; they must include
/// `host` and `x-amz-date` and be sent exactly as given. `canonical_uri` is
/// the path already URI-encoded, as it goes on the wire.
#[allow(clippy::too_many_arguments)]
pub fn authorization(
    method: &str,
    canonical_uri: &str,
    headers: &[(&str, &str)],
    payload_hash: &str,
    amz_date: &str,
    region: &str,
    service: &str,
    access_key_id: &str,
    secret: &str,
) -> String {
    let mut sorted: Vec<(&str, String)> = headers
        .iter()
        .map(|(k, v)| (*k, v.split_whitespace().collect::<Vec<_>>().join(" ")))
        .collect();
    sorted.sort_by(|a, b| a.0.cmp(b.0));
    let signed: Vec<&str> = sorted.iter().map(|(k, _)| *k).collect();
    let signed = signed.join(";");
    let canonical_headers: String = sorted.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    // The query string is always empty here: every request names one object.
    let canonical_request =
        format!("{method}\n{canonical_uri}\n\n{canonical_headers}\n{signed}\n{payload_hash}");

    let date = &amz_date[..8];
    let scope = format!("{date}/{region}/{service}/aws4_request");
    let string_to_sign = format!(
        "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
        hex_sha256(canonical_request.as_bytes())
    );
    let mut key = hmac(format!("AWS4{secret}").as_bytes(), date.as_bytes());
    for part in [region, service, "aws4_request"] {
        key = hmac(&key, part.as_bytes());
    }
    let signature = hex(&hmac(&key, string_to_sign.as_bytes()));
    format!(
        "AWS4-HMAC-SHA256 Credential={access_key_id}/{scope}, SignedHeaders={signed}, Signature={signature}"
    )
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC takes a key of any length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Percent-encode a path for the wire and the canonical request alike: every
/// byte outside RFC 3986's unreserved set, except `/`, becomes `%XX`. S3
/// signs the path encoded once, as sent, with no dot-segment normalisation.
pub fn uri_encode_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Lowercase hex of the SHA-256 of `bytes`.
pub fn hex_sha256(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// `YYYYMMDDTHHMMSSZ` for a Unix time in seconds.
pub fn amz_date(unix: u64) -> String {
    let (days, secs) = (unix / 86_400, unix % 86_400);
    // Howard Hinnant's civil_from_days, for days since 1970-01-01.
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // AWS's published SigV4 test suite (awslabs/aws-c-auth,
    // tests/aws-signing-test-suite/v4), all signed at 20150830T123600Z in
    // us-east-1 for the service "service".
    const AKID: &str = "AKIDEXAMPLE";
    const SECRET: &str = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";
    const DATE: &str = "20150830T123600Z";
    const EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    fn sign(method: &str, uri: &str, headers: &[(&str, &str)], payload: &str) -> String {
        authorization(
            method,
            uri,
            headers,
            payload,
            DATE,
            "us-east-1",
            "service",
            AKID,
            SECRET,
        )
    }

    #[test]
    fn get_vanilla() {
        assert_eq!(
            sign(
                "GET",
                "/",
                &[("host", "example.amazonaws.com"), ("x-amz-date", DATE)],
                EMPTY
            ),
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, \
             SignedHeaders=host;x-amz-date, \
             Signature=5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
        );
    }

    #[test]
    fn get_utf8_path_is_signed_percent_encoded() {
        assert_eq!(
            sign(
                "GET",
                &uri_encode_path("/\u{1234}"),
                &[("host", "example.amazonaws.com"), ("x-amz-date", DATE)],
                EMPTY
            ),
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, \
             SignedHeaders=host;x-amz-date, \
             Signature=8318018e0b0f223aa2bbf98705b62bb787dc9c0e678f255a891fd03141be5d85"
        );
    }

    #[test]
    fn post_x_www_form_urlencoded_signs_the_body_and_sorts_headers() {
        let body = hex_sha256(b"Param1=value1");
        assert_eq!(
            body,
            "9095672bbd1f56dfc5b65f3e153adc8731a4a654192329106275f4c7b24d0b6e"
        );
        assert_eq!(
            sign(
                "POST",
                "/",
                // Deliberately out of order: signing sorts them.
                &[
                    ("x-amz-date", DATE),
                    ("host", "example.amazonaws.com"),
                    ("content-type", "application/x-www-form-urlencoded"),
                    ("x-amz-content-sha256", &body),
                    ("content-length", "13"),
                ],
                &body
            ),
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, \
             SignedHeaders=content-length;content-type;host;x-amz-content-sha256;x-amz-date, \
             Signature=d3875051da38690788ef43de4db0d8f280229d82040bfac253562e56c3f20e0b"
        );
    }

    #[test]
    fn a_path_keeps_its_slashes_and_encodes_the_rest() {
        assert_eq!(
            uri_encode_path("/b/tasqx/snapshot.age"),
            "/b/tasqx/snapshot.age"
        );
        assert_eq!(uri_encode_path("/b/a b+c~_-."), "/b/a%20b%2Bc~_-.");
    }

    #[test]
    fn amz_date_spells_utc() {
        assert_eq!(amz_date(1_440_938_160), DATE);
        assert_eq!(amz_date(0), "19700101T000000Z");
        // A leap day.
        assert_eq!(amz_date(1_709_210_096), "20240229T123456Z");
    }
}
