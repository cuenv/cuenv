//! AWS Signature Version 4 for S3 requests, header authentication only.
//!
//! Written on the `hmac` and `sha2` crates already in the workspace instead
//! of `aws-sigv4`, which would bring in the aws-smithy and credential crates.
//! Every request signs its full payload (`x-amz-content-sha256`); presigned
//! (query) authentication is never used. The tests check the signer against
//! signatures computed by botocore, AWS's own implementation, including the
//! S3 documentation's examples.

use std::fmt;
use std::fmt::Write as _;

use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use sha2::{Digest, Sha256};

type HmacSha256 = Hmac<Sha256>;

/// The algorithm name in the `Authorization` header.
const ALGORITHM: &str = "AWS4-HMAC-SHA256";

/// Static or temporary credentials. `Debug` never shows a value.
#[derive(Clone)]
pub struct Credentials {
    /// Access key identifier.
    pub access_key_id: String,
    /// Secret access key.
    pub secret_access_key: String,
    /// Session token of temporary credentials.
    pub session_token: Option<String>,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Credentials")
            .field("access_key_id", &"<redacted>")
            .field("secret_access_key", &"<redacted>")
            .field(
                "session_token",
                &self.session_token.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

impl Credentials {
    /// Every secret value, for redaction from messages.
    pub fn secrets(&self) -> impl Iterator<Item = &str> {
        [
            Some(self.access_key_id.as_str()),
            Some(self.secret_access_key.as_str()),
            self.session_token.as_deref(),
        ]
        .into_iter()
        .flatten()
    }
}

/// One request to sign.
#[derive(Debug, Clone, Copy)]
pub struct SigningRequest<'request> {
    /// HTTP method, upper case.
    pub method: &'request str,
    /// The `Host` header value (`host` or `host:port`).
    pub host: &'request str,
    /// The canonical URI: the request path, each segment already
    /// URI-encoded exactly once.
    pub path: &'request str,
    /// Query parameters, not encoded.
    pub query: &'request [(String, String)],
    /// Further headers to sign, as sent.
    pub headers: &'request [(String, String)],
    /// Hex SHA-256 of the payload.
    pub payload_sha256: &'request str,
    /// When the request is signed.
    pub time: DateTime<Utc>,
    /// Signing region (`auto` for R2).
    pub region: &'request str,
}

/// The headers a signed request carries, `Authorization` last.
pub type SignedHeaders = Vec<(String, String)>;

/// Hex SHA-256 of `bytes`.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn hmac(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = <HmacSha256 as KeyInit>::new_from_slice(key)
        .unwrap_or_else(|_| unreachable!("HMAC accepts keys of any length"));
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

/// URI-encode `value` as SigV4 requires: unreserved characters stay, every
/// other byte becomes `%XX` (upper case); `/` stays only when
/// `keep_slash`.
#[must_use]
pub fn uri_encode(value: &str, keep_slash: bool) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'_' | b'.' | b'~')
            || (keep_slash && byte == b'/')
        {
            encoded.push(char::from(byte));
        } else {
            let _ = write!(encoded, "%{byte:02X}");
        }
    }
    encoded
}

/// The canonical query string: names and values encoded, sorted by name
/// then value.
#[must_use]
pub fn canonical_query(query: &[(String, String)]) -> String {
    let mut pairs: Vec<(String, String)> = query
        .iter()
        .map(|(name, value)| (uri_encode(name, false), uri_encode(value, false)))
        .collect();
    pairs.sort();
    pairs
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("&")
}

/// Trim a header value and collapse runs of spaces, as SigV4 requires.
fn canonical_value(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The headers that are signed (lower-case names, sorted) for `request`,
/// including the ones the signer adds.
fn headers_to_sign(
    request: &SigningRequest<'_>,
    credentials: &Credentials,
    amz_date: &str,
) -> Vec<(String, String)> {
    let mut headers: Vec<(String, String)> = request
        .headers
        .iter()
        .map(|(name, value)| (name.to_ascii_lowercase(), canonical_value(value)))
        .collect();
    headers.push(("host".to_string(), request.host.to_string()));
    headers.push((
        "x-amz-content-sha256".to_string(),
        request.payload_sha256.to_string(),
    ));
    headers.push(("x-amz-date".to_string(), amz_date.to_string()));
    if let Some(token) = &credentials.session_token {
        headers.push(("x-amz-security-token".to_string(), token.clone()));
    }
    headers.sort();
    headers
}

/// The canonical request of `request`.
fn canonical_request(request: &SigningRequest<'_>, headers: &[(String, String)]) -> String {
    let canonical_headers = headers
        .iter()
        .fold(String::new(), |mut lines, (name, value)| {
            let _ = writeln!(lines, "{name}:{value}");
            lines
        });
    let signed: Vec<&str> = headers.iter().map(|(name, _)| name.as_str()).collect();
    format!(
        "{}\n{}\n{}\n{canonical_headers}\n{}\n{}",
        request.method,
        request.path,
        canonical_query(request.query),
        signed.join(";"),
        request.payload_sha256
    )
}

/// Sign `request`. Returns the headers to send besides the request's own:
/// `x-amz-content-sha256`, `x-amz-date`, `x-amz-security-token` (with a
/// session token) and `Authorization`.
#[must_use]
pub fn sign(request: &SigningRequest<'_>, credentials: &Credentials) -> SignedHeaders {
    let amz_date = request.time.format("%Y%m%dT%H%M%SZ").to_string();
    let date = request.time.format("%Y%m%d").to_string();
    let headers = headers_to_sign(request, credentials, &amz_date);
    let canonical = canonical_request(request, &headers);
    let scope = format!("{date}/{}/s3/aws4_request", request.region);
    let string_to_sign = format!(
        "{ALGORITHM}\n{amz_date}\n{scope}\n{}",
        sha256_hex(canonical.as_bytes())
    );
    let key = [date.as_str(), request.region, "s3", "aws4_request"]
        .iter()
        .fold(
            format!("AWS4{}", credentials.secret_access_key).into_bytes(),
            |key, part| hmac(&key, part.as_bytes()),
        );
    let signature = hex::encode(hmac(&key, string_to_sign.as_bytes()));
    let signed_names: Vec<&str> = headers.iter().map(|(name, _)| name.as_str()).collect();
    let mut added = vec![
        (
            "x-amz-content-sha256".to_string(),
            request.payload_sha256.to_string(),
        ),
        ("x-amz-date".to_string(), amz_date),
    ];
    if let Some(token) = &credentials.session_token {
        added.push(("x-amz-security-token".to_string(), token.clone()));
    }
    added.push((
        "authorization".to_string(),
        format!(
            "{ALGORITHM} Credential={}/{scope}, SignedHeaders={}, Signature={signature}",
            credentials.access_key_id,
            signed_names.join(";")
        ),
    ));
    added
}

#[cfg(test)]
mod tests {
    use super::*;

    fn credentials(token: Option<&str>) -> Credentials {
        Credentials {
            access_key_id: "AKIAIOSFODNN7EXAMPLE".into(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
            session_token: token.map(ToString::to_string),
        }
    }

    fn time() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2013-05-24T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn pairs(values: &[(&str, &str)]) -> Vec<(String, String)> {
        values
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect()
    }

    struct Case<'case> {
        method: &'case str,
        host: &'case str,
        path: &'case str,
        query: &'case [(&'case str, &'case str)],
        headers: &'case [(&'case str, &'case str)],
        body: &'case str,
        region: &'case str,
        token: Option<&'case str>,
    }

    fn signature(case: &Case<'_>) -> String {
        let query = pairs(case.query);
        let headers = pairs(case.headers);
        let payload = sha256_hex(case.body.as_bytes());
        let signed = sign(
            &SigningRequest {
                method: case.method,
                host: case.host,
                path: case.path,
                query: &query,
                headers: &headers,
                payload_sha256: &payload,
                time: time(),
                region: case.region,
            },
            &credentials(case.token),
        );
        let authorization = &signed.last().unwrap().1;
        authorization
            .rsplit_once("Signature=")
            .unwrap()
            .1
            .to_string()
    }

    /// The first three are the S3 documentation's GET Object, PUT Object and
    /// GET Bucket (List Objects) examples; the rest were computed by
    /// botocore 1.43 (`S3SigV4Auth`) for the same requests.
    #[test]
    fn signatures_match_botocore() {
        let cases = [
            (
                Case {
                    method: "GET",
                    host: "examplebucket.s3.amazonaws.com",
                    path: "/test.txt",
                    query: &[],
                    headers: &[("Range", "bytes=0-9")],
                    body: "",
                    region: "us-east-1",
                    token: None,
                },
                "f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41",
            ),
            (
                Case {
                    method: "PUT",
                    host: "examplebucket.s3.amazonaws.com",
                    path: &format!("/{}", uri_encode("test$file.text", false)),
                    query: &[],
                    headers: &[
                        ("Date", "Fri, 24 May 2013 00:00:00 GMT"),
                        ("x-amz-storage-class", "REDUCED_REDUNDANCY"),
                    ],
                    body: "Welcome to Amazon S3.",
                    region: "us-east-1",
                    token: None,
                },
                "98ad721746da40c64f1a55b78f14c238d841ea1380cd77a1b5971af0ece108bd",
            ),
            (
                Case {
                    method: "GET",
                    host: "examplebucket.s3.amazonaws.com",
                    path: "/",
                    query: &[("max-keys", "2"), ("prefix", "J")],
                    headers: &[],
                    body: "",
                    region: "us-east-1",
                    token: None,
                },
                "34b48302e7b5fa45bde8084f4b7868a86f0a534bc59db6670ed5711ef69dc6f7",
            ),
            (
                Case {
                    method: "PUT",
                    host: "abc.r2.cloudflarestorage.com",
                    path: "/state-bucket/github.com/acme/infra/_cuenv/web/_default/state.json",
                    query: &[],
                    headers: &[
                        ("If-Match", "\"0123abcd\""),
                        ("Content-Type", "application/json"),
                    ],
                    body: "{\"format\":{\"read\":1,\"write\":1}}",
                    region: "auto",
                    token: None,
                },
                "48c79c4fe959a49f4c3f78d656a2a1fbc21dc925a7798cb8af5f98300127b504",
            ),
            (
                Case {
                    method: "PUT",
                    host: "state-bucket.s3.eu-west-2.amazonaws.com",
                    path: "/_locks/github.com/acme/infra/_cuenv/web/Prod/lock-1.json",
                    query: &[],
                    headers: &[("If-None-Match", "*")],
                    body: "{}",
                    region: "eu-west-2",
                    token: Some("FQoGZXIvYXdzEJr//////////session"),
                },
                "60be7556b5eda01631d30630fd0250fba623a7e5a30ab04de828762d57dc403c",
            ),
            (
                Case {
                    method: "GET",
                    host: "127.0.0.1:9000",
                    path: "/state/",
                    query: &[
                        ("list-type", "2"),
                        ("prefix", "_locks/github.com/"),
                        ("continuation-token", "a/b="),
                        ("encoding-type", "url"),
                    ],
                    headers: &[],
                    body: "",
                    region: "us-east-1",
                    token: None,
                },
                "df0f2e5a585653f7b1735abae6228a3c52cbc9289d6768d3cc26c82e946f9361",
            ),
        ];
        for (case, expected) in &cases {
            assert_eq!(&signature(case), expected, "{} {}", case.method, case.path);
        }
    }

    #[test]
    fn the_authorization_header_names_the_scope_and_signed_headers() {
        let payload = sha256_hex(b"");
        let signed = sign(
            &SigningRequest {
                method: "GET",
                host: "examplebucket.s3.amazonaws.com",
                path: "/test.txt",
                query: &[],
                headers: &pairs(&[("Range", "bytes=0-9")]),
                payload_sha256: &payload,
                time: time(),
                region: "us-east-1",
            },
            &credentials(Some("token")),
        );
        let names: Vec<&str> = signed.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            [
                "x-amz-content-sha256",
                "x-amz-date",
                "x-amz-security-token",
                "authorization"
            ]
        );
        let authorization = &signed[3].1;
        assert!(authorization.starts_with(
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, \
             SignedHeaders=host;range;x-amz-content-sha256;x-amz-date;x-amz-security-token, \
             Signature="
        ));
    }

    #[test]
    fn encoding_keeps_only_unreserved_characters() {
        assert_eq!(uri_encode("a-b_c.d~e/f", true), "a-b_c.d~e/f");
        assert_eq!(uri_encode("a/b=c d", false), "a%2Fb%3Dc%20d");
        assert_eq!(uri_encode("é", false), "%C3%A9");
        assert_eq!(
            canonical_query(&pairs(&[("b", "2"), ("a", "z"), ("a", "y")])),
            "a=y&a=z&b=2"
        );
    }

    #[test]
    fn debug_output_never_shows_a_credential() {
        let shown = format!("{:?}", credentials(Some("session-secret")));
        assert!(!shown.contains("AKIA"), "{shown}");
        assert!(!shown.contains("wJalr"), "{shown}");
        assert!(!shown.contains("session-secret"), "{shown}");
    }
}
