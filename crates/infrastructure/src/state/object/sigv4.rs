//! AWS Signature Version 4 for S3 requests, header authentication only,
//! through `aws-sigv4`, AWS's own signer.
//!
//! This module adapts the store's requests to it: S3 signs the path as sent
//! (single percent-encoding, no `.`/`..` normalisation) and every request
//! signs its full payload (`x-amz-content-sha256`). Presigned (query)
//! authentication is never used. The tests check the result against the S3
//! documentation's examples and signatures computed by botocore.

use std::fmt;
use std::time::SystemTime;

use aws_sigv4::http_request::{
    PayloadChecksumKind, PercentEncodingMode, SignableBody, SignableRequest, SignatureLocation,
    SigningSettings, UriPathNormalizationMode, sign as sign_request,
};
use aws_sigv4::sign::v4;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

use crate::error::{InfrastructureError, Result};

/// Bytes SigV4 encodes: everything but unreserved characters.
const ENCODED: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

/// [`ENCODED`] without `/`, for object keys in a path.
const ENCODED_PATH: &AsciiSet = &ENCODED.remove(b'/');

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
    /// The full request URL exactly as sent: path segments and query
    /// already encoded with [`uri_encode`] and [`canonical_query`].
    pub url: &'request str,
    /// Further headers to sign, as sent.
    pub headers: &'request [(String, String)],
    /// The payload.
    pub body: &'request [u8],
    /// When the request is signed.
    pub time: SystemTime,
    /// Signing region (`auto` for R2).
    pub region: &'request str,
}

/// The headers a signed request carries besides its own.
pub type SignedHeaders = Vec<(String, String)>;

/// URI-encode `value` as SigV4 requires: unreserved characters stay, every
/// other byte becomes `%XX` (upper case); `/` stays only when
/// `keep_slash`.
#[must_use]
pub fn uri_encode(value: &str, keep_slash: bool) -> String {
    let set = if keep_slash { ENCODED_PATH } else { ENCODED };
    utf8_percent_encode(value, set).to_string()
}

/// The query string: names and values encoded, sorted by name then value,
/// which is also the canonical form SigV4 signs.
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

/// Sign `request`. Returns the headers to send besides the request's own:
/// `x-amz-content-sha256`, `x-amz-date`, `x-amz-security-token` (with a
/// session token) and `authorization`.
///
/// # Errors
///
/// Returns [`InfrastructureError::State`] when the request cannot be signed
/// (a URL or header the signer rejects). The message names neither the URL
/// nor a credential.
pub fn sign(request: &SigningRequest<'_>, credentials: &Credentials) -> Result<SignedHeaders> {
    let identity = aws_credential_types::Credentials::new(
        credentials.access_key_id.clone(),
        credentials.secret_access_key.clone(),
        credentials.session_token.clone(),
        None,
        "cuenv",
    )
    .into();
    let mut settings = SigningSettings::default();
    settings.percent_encoding_mode = PercentEncodingMode::Single;
    settings.payload_checksum_kind = PayloadChecksumKind::XAmzSha256;
    settings.uri_path_normalization_mode = UriPathNormalizationMode::Disabled;
    settings.signature_location = SignatureLocation::Headers;
    let params = v4::SigningParams::builder()
        .identity(&identity)
        .region(request.region)
        .name("s3")
        .time(request.time)
        .settings(settings)
        .build()
        .map_err(|_| InfrastructureError::state("object storage: invalid signing parameters"))?
        .into();
    let signable = SignableRequest::new(
        request.method,
        request.url,
        request
            .headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
        SignableBody::Bytes(request.body),
    )
    .map_err(|_| InfrastructureError::state("object storage: the request URL cannot be signed"))?;
    let (instructions, _) = sign_request(signable, &params)
        .map_err(|_| InfrastructureError::state("object storage: the request cannot be signed"))?
        .into_parts();
    Ok(instructions
        .headers()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect())
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

    fn time() -> SystemTime {
        // 2013-05-24T00:00:00Z, the S3 documentation's example time.
        SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_369_353_600)
    }

    fn url(host: &str, path: &str, query: &[(String, String)]) -> String {
        let query = canonical_query(query);
        if query.is_empty() {
            format!("https://{host}{path}")
        } else {
            format!("https://{host}{path}?{query}")
        }
    }

    fn authorization(signed: &SignedHeaders) -> &str {
        signed
            .iter()
            .find(|(name, _)| name == "authorization")
            .map(|(_, value)| value.as_str())
            .unwrap()
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
        let url = url(case.host, case.path, &query);
        let signed = sign(
            &SigningRequest {
                method: case.method,
                url: &url,
                headers: &headers,
                body: case.body.as_bytes(),
                time: time(),
                region: case.region,
            },
            &credentials(case.token),
        )
        .unwrap();
        authorization(&signed)
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
        let signed = sign(
            &SigningRequest {
                method: "GET",
                url: "https://examplebucket.s3.amazonaws.com/test.txt",
                headers: &pairs(&[("Range", "bytes=0-9")]),
                body: b"",
                time: time(),
                region: "us-east-1",
            },
            &credentials(Some("token")),
        )
        .unwrap();
        let mut names: Vec<&str> = signed.iter().map(|(name, _)| name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            [
                "authorization",
                "x-amz-content-sha256",
                "x-amz-date",
                "x-amz-security-token",
            ]
        );
        assert!(authorization(&signed).starts_with(
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request, \
             SignedHeaders=host;range;x-amz-content-sha256;x-amz-date;x-amz-security-token, \
             Signature="
        ));
    }

    #[test]
    fn dot_segments_are_signed_as_sent() {
        // S3 keys may hold `.` segments; the signer must not normalise them
        // away, or it signs a different path than the one sent.
        let sign_path = |path: &str| {
            let url = format!("https://examplebucket.s3.amazonaws.com{path}");
            let signed = sign(
                &SigningRequest {
                    method: "GET",
                    url: &url,
                    headers: &[],
                    body: b"",
                    time: time(),
                    region: "us-east-1",
                },
                &credentials(None),
            )
            .unwrap();
            authorization(&signed).to_string()
        };
        assert_ne!(sign_path("/a/./b"), sign_path("/a/b"));
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
