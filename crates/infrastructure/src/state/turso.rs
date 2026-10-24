//! Turso (libSQL) state store over the Hrana HTTP protocol.
//!
//! Talks to `POST {url}/v2/pipeline` with bearer-token authentication, so it works
//! against Turso Cloud databases and self-hosted `sqld` alike without a
//! native libSQL dependency. What is stored, and how writes are fenced, is
//! [`super::sql`]'s; this module only carries statements to the database.
//!
//! Transient failures (connection errors, timeouts, HTTP 429 and 5xx,
//! `SQLITE_BUSY`) are retried with exponential backoff; every statement issued
//! is safe to repeat.
//!
//! Transport: redirects are never followed, response bodies are read only up
//! to a fixed size, and a plaintext loopback URL (a local `sqld`) is always
//! contacted directly, never through an `HTTP_PROXY` that would see the token.

use std::fmt;
use std::iter;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use serde::{Deserialize, Serialize};

use super::http::{
    self, BodyPrefix, MAXIMUM_ERROR_BODY_BYTES, MAXIMUM_RESPONSE_BYTES, REQUEST_TIMEOUT,
};
use super::sql::{
    Attempted, Dialect, Disclosure, ExecuteResult, Failure, FailureKind, SqlStateStore,
    SqlTransport, SqlValue, Statement, StatementError, TransactionResult, sql_state_store,
};
use crate::error::{InfrastructureError, Result, describe_json_error, strip_control_characters};

/// The backend's name in messages.
const BACKEND: &str = "Turso";

/// Connection settings for a Turso database.
#[derive(Clone)]
pub struct TursoConfiguration {
    /// Database URL: `libsql://`, `https://` or `wss://`; `http://` and `ws://`
    /// are accepted only for loopback hosts (a local `sqld`).
    pub url: String,
    /// Database authentication token. Optional for unauthenticated local `sqld`.
    pub authentication_token: Option<String>,
}

impl fmt::Debug for TursoConfiguration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TursoConfiguration")
            .field("url", &self.url)
            .field(
                "authentication_token",
                &self.authentication_token.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

/// [`crate::state::StateStore`] backed by a remote Turso database.
#[derive(Clone)]
pub struct TursoStateStore {
    pub(super) inner: SqlStateStore<Hrana>,
}

impl fmt::Debug for TursoStateStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let transport = &self.inner.transport;
        formatter
            .debug_struct("TursoStateStore")
            .field("pipeline_url", &transport.pipeline_url.as_str())
            .field(
                "authentication_token",
                &transport
                    .authentication_token
                    .as_ref()
                    .map(|_| "<redacted>"),
            )
            .field("retry_policy", &self.inner.retry_policy)
            .field("migration_wait", &self.inner.migration_wait)
            .field("maximum_response_bytes", &transport.maximum_response_bytes)
            .finish_non_exhaustive()
    }
}

impl TursoStateStore {
    /// Create a store for the given database.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::Configuration`] for unsupported or unsafe
    /// URLs (plaintext `http://` or `ws://` to a non-loopback host, a missing
    /// host, or embedded credentials) and [`InfrastructureError::State`] if the
    /// HTTP client cannot be built.
    pub fn new(configuration: TursoConfiguration) -> Result<Self> {
        Self::with_timeout(configuration, REQUEST_TIMEOUT)
    }

    /// Check a database URL against the same rules as [`Self::new`] without
    /// creating a store: nothing is sent, and no HTTP client is built, so the
    /// check works where the platform has no root certificates to load.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::Configuration`] for the URLs
    /// [`Self::new`] refuses.
    pub fn validate_url(url: &str) -> Result<()> {
        pipeline_url(url).map(drop)
    }

    fn with_timeout(configuration: TursoConfiguration, timeout: Duration) -> Result<Self> {
        let pipeline_url = pipeline_url(&configuration.url)?;
        let client = http::client(timeout, &pipeline_url)?;
        Ok(Self {
            inner: SqlStateStore::new(Hrana {
                client,
                pipeline_url,
                authentication_token: configuration
                    .authentication_token
                    .filter(|token| !token.is_empty()),
                maximum_response_bytes: MAXIMUM_RESPONSE_BYTES,
            }),
        })
    }

    /// The identity recovery files are bound to: the database endpoint.
    fn backend_identity(&self) -> String {
        use sha2::Digest;
        let mut digest = sha2::Sha256::new();
        digest.update(b"cuenv-infrastructure-turso-recovery-v1\0");
        digest.update(
            backend_endpoint(&self.inner.transport.pipeline_url)
                .as_str()
                .as_bytes(),
        );
        format!("{:x}", digest.finalize())
    }
}

sql_state_store!(TursoStateStore);

/// Statements carried to a Turso database over Hrana.
#[derive(Clone)]
pub(super) struct Hrana {
    client: reqwest::Client,
    pub(super) pipeline_url: reqwest::Url,
    authentication_token: Option<String>,
    /// Largest successful response body read, in bytes.
    pub(super) maximum_response_bytes: usize,
}

impl Hrana {
    /// Send one pipeline request, without retrying.
    ///
    /// An error response is described by its HTTP status and Hrana error
    /// code; its body is quoted only when `disclosure` allows it, because a
    /// server may echo the request (and the state it carries) back.
    async fn send(
        &self,
        body: &PipelineBody<'_>,
        disclosure: Disclosure,
    ) -> Attempted<PipelineResponse> {
        let mut request = self.client.post(self.pipeline_url.clone()).json(body);
        if let Some(token) = &self.authentication_token {
            request = request.bearer_auth(token);
        }
        let mut response = request
            .send()
            .await
            .map_err(|error| http::transport_failure(BACKEND, &error))?;
        let status = response.status();
        if !status.is_success() {
            let total_bytes = response.content_length();
            let retry_after = http::retry_after(&response);
            let body = http::read_body_prefix(&mut response, MAXIMUM_ERROR_BODY_BYTES).await;
            let mut failure = Failure::new(
                if http::status_is_transient(status) {
                    FailureKind::Transient
                } else {
                    FailureKind::Permanent
                },
                describe_http_error(&HttpError {
                    status,
                    body: &body,
                    total_bytes,
                    disclosure,
                }),
            );
            failure.retry_after = retry_after;
            return Err(failure);
        }
        let bytes =
            http::read_whole_body(BACKEND, &mut response, self.maximum_response_bytes).await?;
        serde_json::from_slice(&bytes).map_err(|error| {
            Failure::permanent(format!(
                "invalid Turso response ({})",
                describe_json_error(&error)
            ))
        })
    }

    /// Execute a conditional batch in one pipeline request, without retrying.
    async fn batch_once(
        &self,
        steps: Vec<BatchStep<'_>>,
        disclosure: Disclosure,
    ) -> Attempted<BatchResult> {
        let response = self
            .send(
                &PipelineBody {
                    baton: None,
                    requests: vec![
                        PipelineRequest::Batch {
                            batch: Batch { steps },
                        },
                        PipelineRequest::Close,
                    ],
                },
                disclosure,
            )
            .await?;
        match response.results.into_iter().next() {
            Some(PipelineResult::Ok {
                response: StreamResponse::Batch { result },
            }) => Ok(result),
            Some(PipelineResult::Error { error }) => Err(statement_failure(&error, disclosure)),
            Some(PipelineResult::Ok { .. }) | None => {
                Err(Failure::permanent("unexpected Turso response to a batch"))
            }
        }
    }
}

#[async_trait]
impl SqlTransport for Hrana {
    const NAME: &'static str = BACKEND;
    const DIALECT: Dialect = Dialect::NATIVE;

    /// Statements run in order, each in its own implicit transaction.
    async fn execute_once(&self, statements: &[Statement]) -> Attempted<Vec<ExecuteResult>> {
        let count = statements.len();
        let disclosure = Disclosure::of(statements);
        let requests = statements
            .iter()
            .map(|statement| PipelineRequest::Execute {
                statement: HranaStatement::of(statement),
            })
            .chain(iter::once(PipelineRequest::Close))
            .collect();
        let response = self
            .send(
                &PipelineBody {
                    baton: None,
                    requests,
                },
                disclosure,
            )
            .await?;

        let mut results = Vec::with_capacity(count);
        for entry in response.results.into_iter().take(count) {
            match entry {
                PipelineResult::Ok {
                    response: StreamResponse::Execute { result },
                } => results.push(result.into_result()),
                PipelineResult::Ok { .. } => {
                    return Err(Failure::permanent("unexpected Turso response type"));
                }
                PipelineResult::Error { error } => {
                    return Err(statement_failure(&error, disclosure));
                }
            }
        }
        if results.len() != count {
            return Err(Failure::permanent(format!(
                "Turso returned {} results for {count} statements",
                results.len()
            )));
        }
        Ok(results)
    }

    /// `BEGIN IMMEDIATE`, the statements, `COMMIT`, then a `ROLLBACK` that
    /// runs only when the commit did not, in one conditional batch.
    async fn transaction_once(&self, statements: &[Statement]) -> Attempted<TransactionResult> {
        let begin = Statement::new("BEGIN IMMEDIATE", Vec::new());
        let commit = Statement::new("COMMIT", Vec::new());
        let rollback = Statement::new("ROLLBACK", Vec::new());
        let all: Vec<&Statement> = iter::once(&begin)
            .chain(statements)
            .chain([&commit, &rollback])
            .collect();
        let commit_index = all.len() - 2;
        let disclosure = Disclosure::of(statements);
        let result = self
            .batch_once(transaction_steps(&all, commit_index), disclosure)
            .await?;
        transaction_result(&result, commit_index)
    }

    fn secret(&self) -> Option<&str> {
        self.authentication_token.as_deref()
    }

    fn statement_failure(&self, error: &StatementError, disclosure: Disclosure) -> Failure {
        statement_failure(
            &HranaError {
                message: error.message.clone(),
                code: error.code.clone(),
            },
            disclosure,
        )
    }
}

/// Steps for a transaction batch: `statements` is `BEGIN`, the body,
/// `COMMIT` at `commit_index`, then `ROLLBACK`. Each step up to the commit
/// runs only if the previous one succeeded; the rollback runs only if the
/// commit did not.
fn transaction_steps<'statement>(
    statements: &[&'statement Statement],
    commit_index: usize,
) -> Vec<BatchStep<'statement>> {
    statements
        .iter()
        .enumerate()
        .map(|(index, statement)| BatchStep {
            condition: match index {
                0 => None,
                index if index <= commit_index => Some(BatchCondition::Ok { step: index - 1 }),
                _ => Some(BatchCondition::Not {
                    condition: Box::new(BatchCondition::Ok { step: commit_index }),
                }),
            },
            statement: HranaStatement::of(statement),
        })
        .collect()
}

/// What a transaction batch says: committed, or which body statement (after
/// `BEGIN`, zero based) the database rejected.
fn transaction_result(result: &BatchResult, commit_index: usize) -> Attempted<TransactionResult> {
    let first_error = result
        .step_errors
        .iter()
        .take(commit_index + 1)
        .enumerate()
        .find_map(|(index, error)| error.as_ref().map(|error| (index, error)));
    if let Some((index, error)) = first_error {
        if error.code.as_deref() == Some("SQLITE_BUSY") {
            return Err(statement_failure(error, Disclosure::Full));
        }
        return Ok(TransactionResult::Failed {
            statement: index.checked_sub(1),
            error: StatementError {
                message: error.message.clone(),
                code: error.code.clone(),
            },
        });
    }
    if result
        .step_results
        .get(commit_index)
        .is_some_and(Option::is_some)
    {
        Ok(TransactionResult::Committed)
    } else {
        Err(Failure::permanent(
            "Turso did not commit the schema migration transaction",
        ))
    }
}

/// Describe a statement error: its code always, its message only when the
/// statement carried no state (a message may echo the arguments).
fn statement_failure(error: &HranaError, disclosure: Disclosure) -> Failure {
    let busy = error
        .code
        .as_deref()
        .is_some_and(|code| code == "SQLITE_BUSY");
    let code = error
        .code
        .as_deref()
        .map_or_else(|| "no code".to_string(), strip_control_characters);
    Failure::new(
        if busy {
            FailureKind::Transient
        } else {
            FailureKind::Statement
        },
        match disclosure {
            Disclosure::Full => format!(
                "Turso statement failed: {} ({code})",
                strip_control_characters(&error.message)
            ),
            Disclosure::CodeOnly => format!(
                "Turso statement failed with {code} (message withheld: the statement carried \
                 resource state)"
            ),
        },
    )
}

/// An HTTP error response to describe.
struct HttpError<'response> {
    status: reqwest::StatusCode,
    body: &'response BodyPrefix,
    total_bytes: Option<u64>,
    disclosure: Disclosure,
}

/// Describe an HTTP error response by its status and Hrana error code; the
/// body itself is quoted (bounded, without control characters) only when
/// the request carried no state.
fn describe_http_error(error: &HttpError<'_>) -> String {
    let status = error.status;
    let code = serde_json::from_slice::<HranaErrorBody>(&error.body.bytes)
        .ok()
        .and_then(|body| body.code)
        .map(|code| format!(" ({})", strip_control_characters(&code)))
        .unwrap_or_default();
    match error.disclosure {
        Disclosure::Full => format!(
            "Turso returned HTTP {status}{code}: {}",
            strip_control_characters(&http::describe_error_body(error.body, error.total_bytes))
        ),
        Disclosure::CodeOnly => format!(
            "Turso returned HTTP {status}{code} (response body withheld: the request carried \
             resource state)"
        ),
    }
}

/// The error body Hrana servers send with an HTTP error status.
#[derive(Debug, Deserialize)]
struct HranaErrorBody {
    #[serde(default)]
    code: Option<String>,
}

/// Convert a database URL into the Hrana v2 pipeline endpoint.
///
/// Plaintext schemes (`http://`, `ws://`) are accepted only for loopback hosts,
/// because the bearer token would otherwise cross the network unencrypted.
/// Error messages never quote the full URL, which may carry secrets.
///
/// The accepted set is exactly the schema's (`#TursoState.url` in
/// `schema/infrastructure.cue`): the URL text is checked against that
/// contract before it is parsed, so the parser's own leniency (trimming,
/// percent-decoding, international names, numeric host forms such as
/// `127.1`) can never widen it.
fn pipeline_url(url: &str) -> Result<reqwest::Url> {
    if url.chars().any(char::is_whitespace) {
        return Err(InfrastructureError::configuration(
            "invalid Turso URL: it must not contain whitespace",
        ));
    }
    let (scheme, remainder) = url.split_once("://").ok_or_else(|| {
        InfrastructureError::configuration(
            "invalid Turso URL: expected libsql://, https:// or wss:// \
             (http:// and ws:// only for a loopback host)",
        )
    })?;
    let scheme = scheme.to_ascii_lowercase();
    let (http_scheme, transport) = match scheme.as_str() {
        "libsql" | "https" | "wss" => ("https", Transport::Encrypted),
        "http" | "ws" => ("http", Transport::Plaintext),
        _ => {
            return Err(InfrastructureError::configuration(
                "unsupported Turso URL scheme; expected libsql://, https:// or wss:// \
                 (http:// and ws:// only for a loopback host)",
            ));
        }
    };
    let parts = UrlParts::split(remainder)?;
    if transport == Transport::Plaintext && !is_loopback_host(parts.host) {
        return Err(InfrastructureError::configuration(
            "Turso URL uses plaintext transport for a non-loopback host, which would send the \
             authentication token unencrypted; non-loopback URLs must use libsql://, https:// \
             or wss://",
        ));
    }
    let mut parsed =
        reqwest::Url::parse(&format!("{http_scheme}://{remainder}")).map_err(|_| {
            InfrastructureError::configuration("invalid Turso URL: malformed URL components")
        })?;
    if parsed.host_str().is_none_or(str::is_empty) {
        return Err(InfrastructureError::configuration(
            "invalid Turso URL: missing host",
        ));
    }
    let path = format!("{}/v2/pipeline", parsed.path().trim_end_matches('/'));
    parsed.set_path(&path);
    Ok(parsed)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Transport {
    Encrypted,
    Plaintext,
}

/// The authority and path of a Turso URL after `scheme://`, checked
/// against the schema's URL contract: a host, an optional port from 1 to
/// 65535 and a path of URL path characters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct UrlParts<'url> {
    /// A DNS name, dotted IPv4 address or bracketed IPv6 address.
    host: &'url str,
}

impl<'url> UrlParts<'url> {
    /// Split and check `remainder`. Errors never quote it, since it may
    /// carry a token.
    fn split(remainder: &'url str) -> Result<Self> {
        let (authority, path) = remainder
            .find('/')
            .map_or((remainder, ""), |index| remainder.split_at(index));
        if authority.contains('@') || remainder.contains(['?', '#']) {
            return Err(InfrastructureError::configuration(
                "Turso URL must not contain credentials, a query or a fragment; pass the token \
                 as the authentication token instead",
            ));
        }
        let (host, port) = if authority.starts_with('[') {
            let end = authority.find(']').ok_or_else(|| {
                InfrastructureError::configuration("invalid Turso URL: unclosed IPv6 address")
            })?;
            let (host, rest) = authority.split_at(end + 1);
            (host, rest)
        } else {
            authority
                .find(':')
                .map_or((authority, ""), |index| authority.split_at(index))
        };
        if host.is_empty() {
            return Err(InfrastructureError::configuration(
                "invalid Turso URL: missing host",
            ));
        }
        if !is_valid_host(host) {
            return Err(InfrastructureError::configuration(
                "invalid Turso URL: the host must be a DNS name (letters, digits, '.' and '-', \
                 starting and ending with a letter or digit), a dotted IPv4 address or a \
                 bracketed IPv6 address",
            ));
        }
        let port_valid = port.is_empty() || port.strip_prefix(':').is_some_and(is_valid_port);
        if !port_valid {
            return Err(InfrastructureError::configuration(
                "invalid Turso URL: the port must be a number from 1 to 65535",
            ));
        }
        if !path.chars().all(is_path_character) {
            return Err(InfrastructureError::configuration(
                "invalid Turso URL: the path may hold only letters, digits and \
                 ._~!$&'()*+,;=:@%/-",
            ));
        }
        Ok(Self { host })
    }
}

/// A DNS name `[A-Za-z0-9]([A-Za-z0-9.-]*[A-Za-z0-9])?` (which covers a
/// dotted IPv4 address) or a bracketed IPv6 address `[` hex, `:`, `.` `]`.
/// International names, `_` and percent-encoding are refused.
fn is_valid_host(host: &str) -> bool {
    if let Some(inner) = host
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
    {
        return inner.parse::<std::net::Ipv6Addr>().is_ok();
    }
    let bytes = host.as_bytes();
    let alphanumeric_edge = |byte: Option<&u8>| byte.is_some_and(u8::is_ascii_alphanumeric);
    let name_characters = alphanumeric_edge(bytes.first())
        && alphanumeric_edge(bytes.last())
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'.' || *byte == b'-');
    // A name that ends in a number is an address, and only the exact dotted
    // decimal form of one is accepted: the URL parser would otherwise read
    // `0177.0.0.1` as octal, `127.1` or `2130706433` as shorthand and
    // `999.1.1.1` as an error.
    name_characters && (!ends_in_a_number(host) || is_dotted_decimal_ipv4(host))
}

/// Whether the last dot-separated label is all digits or `0x` and hexadecimal
/// digits: the URL standard then treats the whole host as an IPv4 address.
fn ends_in_a_number(host: &str) -> bool {
    let last = host.rsplit('.').next().unwrap_or(host);
    let decimal = !last.is_empty() && last.bytes().all(|byte| byte.is_ascii_digit());
    let hexadecimal = last
        .strip_prefix("0x")
        .or_else(|| last.strip_prefix("0X"))
        .is_some_and(|digits| digits.bytes().all(|byte| byte.is_ascii_hexdigit()));
    decimal || hexadecimal
}

/// Four decimal octets from 0 to 255 without leading zeros.
fn is_dotted_decimal_ipv4(text: &str) -> bool {
    let octets: Vec<&str> = text.split('.').collect();
    octets.len() == 4
        && octets.iter().all(|octet| {
            (1..=3).contains(&octet.len())
                && octet.bytes().all(|byte| byte.is_ascii_digit())
                && (octet.len() == 1 || !octet.starts_with('0'))
                && octet.parse::<u16>().is_ok_and(|value| value <= 255)
        })
}

/// A port from 1 to 65535 in plain decimal, without a leading zero.
fn is_valid_port(digits: &str) -> bool {
    !digits.is_empty()
        && digits.len() <= 5
        && !digits.starts_with('0')
        && digits.bytes().all(|byte| byte.is_ascii_digit())
        && digits
            .parse::<u32>()
            .is_ok_and(|port| (1..=65_535).contains(&port))
}

fn is_path_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || "._~!$&'()*+,;=:@%/-".contains(character)
}

/// Loopback, decided from the host text alone: `localhost` in any case,
/// `127.a.b.c` in dotted decimal, exactly `[::1]`, or `[::ffff:127.a.b.c]`.
/// Other spellings that resolve to loopback (`127.1`, `0x7f.1`,
/// `2130706433`, `[0:0:0:0:0:0:0:1]`, `[::ffff:7f00:1]`) are refused, as
/// the schema refuses them.
fn is_loopback_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") || host == "[::1]" {
        return true;
    }
    let mapped_prefix = "[::ffff:";
    if host.len() > mapped_prefix.len()
        && host.is_char_boundary(mapped_prefix.len())
        && host[..mapped_prefix.len()].eq_ignore_ascii_case(mapped_prefix)
        && let Some(address) = host[mapped_prefix.len()..].strip_suffix(']')
    {
        return is_loopback_ipv4_text(address);
    }
    is_loopback_ipv4_text(host)
}

/// `127.a.b.c` with decimal octets from 0 to 255 and no leading zeros.
fn is_loopback_ipv4_text(text: &str) -> bool {
    text.starts_with("127.") && is_dotted_decimal_ipv4(text)
}

/// The endpoint that identifies a backend: every spelling of the machine's
/// own loopback interface (`localhost`, `127.0.0.1`, `[::1]`) names one.
///
/// A recovery file is bound to the server it was saved for. A local `sqld`
/// is reached by whichever of those names the operator typed, and the name
/// can change between the run that saved a file and the one that recovers it.
/// Treating them as one identity is safe because they can only reach a
/// process on this machine on the same port; it cannot make two machines
/// look alike. The residual case is two different servers on the same port,
/// one listening only on IPv4 and one only on IPv6; the compare-and-swap
/// check on the stored record still guards the write. Other loopback
/// addresses (`127.0.0.2`) stay distinct.
fn backend_endpoint(url: &reqwest::Url) -> reqwest::Url {
    // The parser has already lowercased domains and written IPv6 addresses
    // in their canonical form, so these three spellings are all there is.
    let is_own_loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    let mut endpoint = url.clone();
    if is_own_loopback && endpoint.set_host(Some("localhost")).is_ok() {
        endpoint
    } else {
        url.clone()
    }
}

// ---------------------------------------------------------------------------
// Hrana over HTTP wire types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct PipelineBody<'body> {
    baton: Option<String>,
    requests: Vec<PipelineRequest<'body>>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum PipelineRequest<'body> {
    Execute {
        #[serde(rename = "stmt")]
        statement: HranaStatement<'body>,
    },
    Batch {
        batch: Batch<'body>,
    },
    Close,
}

#[derive(Debug, Serialize)]
struct Batch<'body> {
    steps: Vec<BatchStep<'body>>,
}

#[derive(Debug, Serialize)]
struct BatchStep<'body> {
    #[serde(skip_serializing_if = "Option::is_none")]
    condition: Option<BatchCondition>,
    #[serde(rename = "stmt")]
    statement: HranaStatement<'body>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum BatchCondition {
    Ok {
        step: usize,
    },
    Not {
        #[serde(rename = "cond")]
        condition: Box<Self>,
    },
}

/// A statement as Hrana sends it.
#[derive(Debug, Serialize)]
struct HranaStatement<'statement> {
    sql: &'statement str,
    #[serde(rename = "args")]
    arguments: Vec<HranaValue>,
    want_rows: bool,
}

impl<'statement> HranaStatement<'statement> {
    fn of(statement: &'statement Statement) -> Self {
        Self {
            sql: &statement.sql,
            arguments: statement.arguments.iter().map(HranaValue::of).collect(),
            want_rows: true,
        }
    }
}

#[derive(Debug, Deserialize)]
struct PipelineResponse {
    results: Vec<PipelineResult>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum PipelineResult {
    Ok { response: StreamResponse },
    Error { error: HranaError },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum StreamResponse {
    Execute {
        result: HranaExecuteResult,
    },
    Batch {
        result: BatchResult,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Default, Deserialize)]
struct HranaExecuteResult {
    #[serde(default)]
    rows: Vec<Vec<HranaValue>>,
    #[serde(default)]
    affected_row_count: u64,
}

impl HranaExecuteResult {
    fn into_result(self) -> ExecuteResult {
        ExecuteResult {
            rows: self
                .rows
                .into_iter()
                .map(|row| row.into_iter().map(HranaValue::into_value).collect())
                .collect(),
            affected_row_count: self.affected_row_count,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct BatchResult {
    #[serde(default)]
    step_results: Vec<Option<HranaExecuteResult>>,
    #[serde(default)]
    step_errors: Vec<Option<HranaError>>,
}

#[derive(Debug, Deserialize)]
struct HranaError {
    message: String,
    #[serde(default)]
    code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum HranaValue {
    Null,
    Integer { value: String },
    Float { value: f64 },
    Text { value: String },
    Blob { base64: String },
}

impl HranaValue {
    /// How Hrana sends a bound value. A blob without bytes and an absent
    /// optional text are null.
    fn of(value: &SqlValue) -> Self {
        match value {
            SqlValue::Integer(value) => Self::Integer {
                value: value.to_string(),
            },
            SqlValue::Float(value) => Self::Float { value: *value },
            SqlValue::Text(value) | SqlValue::OptionalText(Some(value)) => Self::Text {
                value: value.clone(),
            },
            SqlValue::Blob(bytes) if !bytes.is_empty() => Self::Blob {
                base64: STANDARD_NO_PAD.encode(bytes),
            },
            SqlValue::Null
            | SqlValue::Blob(_)
            | SqlValue::OptionalText(None)
            | SqlValue::Unreadable => Self::Null,
        }
    }

    /// The value a row holds. A blob that is not base64 is unreadable (the
    /// decoder's message would quote the offending byte).
    fn into_value(self) -> SqlValue {
        match self {
            Self::Null => SqlValue::Null,
            Self::Integer { value } => value
                .parse()
                .map_or(SqlValue::Unreadable, SqlValue::Integer),
            Self::Float { value } => SqlValue::Float(value),
            Self::Text { value } => SqlValue::Text(value),
            Self::Blob { base64 } => {
                let trimmed = base64.trim_end_matches('=');
                STANDARD_NO_PAD
                    .decode(trimmed)
                    .or_else(|_| STANDARD.decode(&base64))
                    .map_or(SqlValue::Unreadable, SqlValue::Blob)
            }
        }
    }
}

#[cfg(test)]
mod tests;
