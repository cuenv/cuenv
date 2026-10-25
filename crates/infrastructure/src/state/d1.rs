//! Cloudflare D1 state store over the Cloudflare REST API.
//!
//! Talks to `POST https://api.cloudflare.com/client/v4/accounts/{account}/d1/database/{database}/raw`
//! with an API token. D1 is SQLite, so what is stored, and how writes are
//! fenced, is [`super::sql`]'s, exactly as for Turso; this module only carries
//! statements to the database.
//!
//! What D1's API allows shapes the requests:
//!
//! - Parameters are bound as text only. Integers are sent in decimal and the
//!   column's affinity stores and compares them as numbers; blobs are sent in
//!   hexadecimal and decoded with `unhex`, and an absent blob or optional text
//!   is sent empty and stored as null ([`DIALECT`]).
//! - A request binds parameters only when it holds one statement. Requests of
//!   several statements (a migration, the schema version check) bind nothing,
//!   and D1 runs them as one transaction: when one fails, none commits. D1
//!   has no `BEGIN`; it executes one request at a time, so a transaction
//!   holds the database as `BEGIN IMMEDIATE` does on Turso.
//! - A rejected statement is an HTTP 400 whose body names the error, so the
//!   body is read before the status decides anything.
//!
//! Transient failures (connection errors, timeouts, HTTP 408, 429 and 5xx,
//! and the D1 errors Cloudflare documents as retryable) are retried with
//! exponential backoff, honouring `Retry-After`; every statement issued is
//! safe to repeat. An overloaded database is not retried: Cloudflare asks
//! for fewer requests.
//!
//! Transport: redirects are never followed, response bodies are read only up
//! to a fixed size, and error messages never name the account or the
//! database (transport errors drop the URL, and Cloudflare's own messages
//! have both identifiers replaced).

use std::fmt;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::http::{self, MAXIMUM_ERROR_BODY_BYTES, MAXIMUM_RESPONSE_BYTES, REQUEST_TIMEOUT};
use super::sql::{
    Attempted, Dialect, Disclosure, ExecuteResult, Failure, FailureKind, SqlStateStore,
    SqlTransport, SqlValue, Statement, StatementError, TransactionResult, sql_state_store,
};
use crate::error::{InfrastructureError, Result, describe_json_error, strip_control_characters};

/// The backend's name in messages.
const BACKEND: &str = "D1";

/// The Cloudflare API every request goes to.
const CLOUDFLARE_API: &str = "https://api.cloudflare.com/client/v4";

/// How D1 binds what it cannot take as a parameter of its own type.
const DIALECT: Dialect = Dialect {
    blob_parameter: "NULLIF(unhex(?), x'')",
    optional_text_parameter: "NULLIF(?, '')",
};

/// The error code D1 gives a statement SQLite rejected.
const SQL_ERROR_CODE: i64 = 7500;

/// Messages of D1 errors Cloudflare documents as worth retrying. An
/// overloaded database is not among them: Cloudflare asks for fewer requests,
/// not more.
const TRANSIENT_MESSAGES: [&str; 6] = [
    "Network connection lost",
    "caused object to be reset",
    "reset because its code was updated",
    "transient issue on remote node",
    "Replica disconnected from primary",
    "client disconnected",
];

/// The largest integer a D1 response carries exactly: its numbers are
/// JavaScript numbers, so integers beyond 2^53 may have lost precision.
const LARGEST_EXACT_INTEGER: u64 = 1 << 53;

/// Connection settings for a D1 database.
#[derive(Clone)]
pub struct D1Configuration {
    /// Cloudflare account identifier: 32 lowercase hexadecimal digits.
    pub account_id: String,
    /// Database identifier: a UUID in lowercase.
    pub database_id: String,
    /// Cloudflare API token allowed to query the database (D1 Edit).
    pub api_token: String,
}

impl fmt::Debug for D1Configuration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("D1Configuration")
            .field("account_id", &self.account_id)
            .field("database_id", &self.database_id)
            .field("api_token", &"<redacted>")
            .finish()
    }
}

/// [`crate::state::StateStore`] backed by a Cloudflare D1 database.
#[derive(Clone)]
pub struct D1StateStore {
    pub(super) inner: SqlStateStore<D1>,
}

impl fmt::Debug for D1StateStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let transport = &self.inner.transport;
        formatter
            .debug_struct("D1StateStore")
            .field("account_id", &transport.account_id)
            .field("database_id", &transport.database_id)
            .field("api_token", &"<redacted>")
            .field("retry_policy", &self.inner.retry_policy)
            .field("migration_wait", &self.inner.migration_wait)
            .field("maximum_response_bytes", &transport.maximum_response_bytes)
            .finish_non_exhaustive()
    }
}

impl D1StateStore {
    /// Create a store for the given database.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::Configuration`] for an account or
    /// database identifier of the wrong form or an empty token, and
    /// [`InfrastructureError::State`] if the HTTP client cannot be built.
    pub fn try_new(configuration: D1Configuration) -> Result<Self> {
        Self::at(configuration, CLOUDFLARE_API, REQUEST_TIMEOUT)
    }

    /// Check the account and database identifiers against the same rules as
    /// [`Self::try_new`] without creating a store: nothing is sent and no HTTP
    /// client is built.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::Configuration`] for the identifiers
    /// [`Self::try_new`] refuses.
    pub fn validate_identifiers(account_id: &str, database_id: &str) -> Result<()> {
        if !is_account_id(account_id) {
            return Err(InfrastructureError::configuration(
                "invalid D1 account identifier: expected the 32 lowercase hexadecimal digits \
                 of a Cloudflare account ID",
            ));
        }
        if !is_database_id(database_id) {
            return Err(InfrastructureError::configuration(
                "invalid D1 database identifier: expected a lowercase UUID such as \
                 0b2c3d4e-5f60-4718-8293-a4b5c6d7e8f9",
            ));
        }
        Ok(())
    }

    /// A store reached through the Cloudflare API at `api`.
    fn at(configuration: D1Configuration, api: &str, timeout: Duration) -> Result<Self> {
        Self::validate_identifiers(&configuration.account_id, &configuration.database_id)?;
        if configuration.api_token.is_empty() {
            return Err(InfrastructureError::configuration(
                "D1 needs a Cloudflare API token; none was given",
            ));
        }
        let endpoint = reqwest::Url::parse(&format!(
            "{api}/accounts/{}/d1/database/{}/raw",
            configuration.account_id, configuration.database_id
        ))
        .map_err(|_| InfrastructureError::configuration("invalid D1 endpoint"))?;
        let client = http::client(timeout, &endpoint)?;
        Ok(Self {
            inner: SqlStateStore::new(D1 {
                client,
                endpoint,
                account_id: configuration.account_id,
                database_id: configuration.database_id,
                api_token: configuration.api_token,
                maximum_response_bytes: MAXIMUM_RESPONSE_BYTES,
            }),
        })
    }

    /// The identity recovery files are bound to: the account and database.
    fn backend_identity(&self) -> String {
        use sha2::Digest;
        let transport = &self.inner.transport;
        let mut digest = sha2::Sha256::new();
        digest.update(b"cuenv-infrastructure-d1-recovery-v1\0");
        digest.update(transport.account_id.as_bytes());
        digest.update(b"/");
        digest.update(transport.database_id.as_bytes());
        format!("{:x}", digest.finalize())
    }
}

sql_state_store!(D1StateStore);

/// Lowercase hexadecimal digits only.
fn is_lowercase_hex(text: &str) -> bool {
    text.bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// 32 lowercase hexadecimal digits.
fn is_account_id(text: &str) -> bool {
    text.len() == 32 && is_lowercase_hex(text)
}

/// A UUID in its lowercase hyphenated form.
fn is_database_id(text: &str) -> bool {
    let groups: Vec<&str> = text.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(group, length)| group.len() == length && is_lowercase_hex(group))
}

/// Statements carried to a D1 database over the Cloudflare API.
#[derive(Clone)]
pub(super) struct D1 {
    client: reqwest::Client,
    endpoint: reqwest::Url,
    account_id: String,
    database_id: String,
    api_token: String,
    /// Largest successful response body read, in bytes.
    pub(super) maximum_response_bytes: usize,
}

/// What a request failed with, before it is described.
#[derive(Debug)]
enum Rejection {
    /// The database rejected a statement (D1's SQL error).
    Statement(StatementError),
    /// Anything else, already classified.
    Other(Failure),
}

impl D1 {
    /// Send `statements` in one request, without retrying, and return one
    /// result per statement. Errors never name the account or the database:
    /// Cloudflare's own messages can quote the request path.
    async fn send(
        &self,
        statements: &[Statement],
        disclosure: Disclosure,
    ) -> std::result::Result<Vec<ExecuteResult>, Rejection> {
        self.request(statements, disclosure)
            .await
            .map_err(|rejection| match rejection {
                Rejection::Statement(mut error) => {
                    error.message = self.anonymize(&error.message);
                    Rejection::Statement(error)
                }
                Rejection::Other(mut failure) => {
                    failure.message = self.anonymize(&failure.message);
                    Rejection::Other(failure)
                }
            })
    }

    /// `message` with the account and database identifiers replaced.
    fn anonymize(&self, message: &str) -> String {
        message
            .replace(&self.account_id, "<account>")
            .replace(&self.database_id, "<database>")
    }

    async fn request(
        &self,
        statements: &[Statement],
        disclosure: Disclosure,
    ) -> std::result::Result<Vec<ExecuteResult>, Rejection> {
        let body = QueryBody::of(statements).map_err(Rejection::Other)?;
        let mut response = self
            .client
            .post(self.endpoint.clone())
            .bearer_auth(&self.api_token)
            .json(&body)
            .send()
            .await
            .map_err(|error| Rejection::Other(transport_failure(error)))?;
        let status = response.status();
        if !status.is_success() {
            let retry_after = http::retry_after(&response);
            let total_bytes = response.content_length();
            let prefix = http::read_body_prefix(&mut response, MAXIMUM_ERROR_BODY_BYTES).await;
            let envelope = serde_json::from_slice::<Envelope>(&prefix.bytes).ok();
            return Err(rejection(&Rejected {
                status,
                error: envelope
                    .as_ref()
                    .and_then(|envelope| envelope.errors.first()),
                body: strip_control_characters(&http::describe_error_body(&prefix, total_bytes)),
                disclosure,
                retry_after,
            }));
        }
        let bytes = http::read_whole_body(BACKEND, &mut response, self.maximum_response_bytes)
            .await
            .map_err(Rejection::Other)?;
        let envelope: Envelope = serde_json::from_slice(&bytes).map_err(|error| {
            Rejection::Other(Failure::permanent(format!(
                "invalid D1 response ({})",
                describe_json_error(&error)
            )))
        })?;
        if !envelope.success || envelope.result.iter().any(|result| !result.success) {
            return Err(rejection(&Rejected {
                status,
                error: envelope.errors.first(),
                body: "the response reports a failure without naming an error".to_string(),
                disclosure,
                retry_after: None,
            }));
        }
        if envelope.result.len() != statements.len() {
            return Err(Rejection::Other(Failure::permanent(format!(
                "D1 returned {} results for {} statements",
                envelope.result.len(),
                statements.len()
            ))));
        }
        Ok(envelope
            .result
            .into_iter()
            .map(QueryResult::into_result)
            .collect())
    }
}

#[async_trait]
impl SqlTransport for D1 {
    const NAME: &'static str = BACKEND;
    const DIALECT: Dialect = DIALECT;

    /// Several statements run as one transaction.
    async fn execute_once(&self, statements: &[Statement]) -> Attempted<Vec<ExecuteResult>> {
        let disclosure = Disclosure::of(statements);
        self.send(statements, disclosure)
            .await
            .map_err(|rejection| match rejection {
                Rejection::Statement(error) => self.statement_failure(&error, disclosure),
                Rejection::Other(failure) => failure,
            })
    }

    async fn transaction_once(&self, statements: &[Statement]) -> Attempted<TransactionResult> {
        match self.send(statements, Disclosure::of(statements)).await {
            Ok(_) => Ok(TransactionResult::Committed),
            // D1 does not say which statement it rejected.
            Err(Rejection::Statement(error)) => Ok(TransactionResult::Failed {
                statement: None,
                error,
            }),
            Err(Rejection::Other(failure)) => Err(failure),
        }
    }

    fn secret(&self) -> Option<&str> {
        Some(&self.api_token)
    }

    fn statement_failure(&self, error: &StatementError, disclosure: Disclosure) -> Failure {
        let message = match disclosure {
            Disclosure::Full => format!(
                "D1 statement failed: {}",
                strip_control_characters(&error.message)
            ),
            Disclosure::CodeOnly => "D1 statement failed (message withheld: the statement \
                                     carried resource state)"
                .to_string(),
        };
        Failure::new(
            FailureKind::Statement,
            if is_too_big(&error.message) {
                format!(
                    "{message}; a value exceeds D1's limit of 2 MB per string, blob or row, so \
                     this resource's state is too large to store in D1"
                )
            } else {
                message
            },
        )
    }
}

/// Whether D1 refused a value as too large.
fn is_too_big(message: &str) -> bool {
    message.contains("SQLITE_TOOBIG") || message.contains("too big")
}

/// Whether a D1 error message names a failure Cloudflare documents as
/// retryable.
fn is_transient_message(message: &str) -> bool {
    TRANSIENT_MESSAGES
        .iter()
        .any(|pattern| message.contains(pattern))
}

/// A failure to reach D1, described without the request URL (which names
/// the account and database).
fn transport_failure(error: reqwest::Error) -> Failure {
    http::transport_failure(BACKEND, &error.without_url())
}

/// A failed D1 response to classify.
struct Rejected<'response> {
    status: reqwest::StatusCode,
    /// The first error the body names, when it is a Cloudflare envelope.
    error: Option<&'response ApiError>,
    /// The body as text (bounded, without control characters), for a body
    /// that is not an envelope.
    body: String,
    disclosure: Disclosure,
    retry_after: Option<Duration>,
}

/// Classify a failed response: a retryable D1 error or HTTP status is
/// transient, D1's SQL error is the statement's, and anything else is
/// permanent. A message is quoted only as `disclosure` allows; the code,
/// and what it is known to mean, always are.
fn rejection(rejected: &Rejected<'_>) -> Rejection {
    let status = rejected.status;
    let transient_message = rejected
        .error
        .is_some_and(|error| is_transient_message(&error.message));
    if let Some(error) = rejected.error
        && error.code == Some(SQL_ERROR_CODE)
        && !transient_message
    {
        return Rejection::Statement(StatementError {
            message: error.message.clone(),
            code: Some(SQL_ERROR_CODE.to_string()),
        });
    }
    let kind = if transient_message || http::status_is_transient(status) {
        FailureKind::Transient
    } else {
        FailureKind::Permanent
    };
    let code = rejected
        .error
        .and_then(|error| error.code)
        .map(|code| format!(" (error {code}{})", code_meaning(code)))
        .unwrap_or_default();
    let detail = match (rejected.disclosure, rejected.error) {
        (Disclosure::Full, Some(error)) => {
            format!(": {}", strip_control_characters(&error.message))
        }
        (Disclosure::Full, None) if rejected.body.is_empty() => String::new(),
        (Disclosure::Full, None) => format!(": {}", rejected.body),
        (Disclosure::CodeOnly, _) => {
            " (response body withheld: the request carried resource state)".to_string()
        }
    };
    let mut failure = Failure::new(kind, format!("D1 returned HTTP {status}{code}{detail}"));
    failure.retry_after = rejected.retry_after;
    Rejection::Other(failure)
}

/// What a Cloudflare API error code is known to mean, for the codes a
/// misconfigured state backend meets.
const fn code_meaning(code: i64) -> &'static str {
    match code {
        10000 => ": the API token was refused",
        7403 => ": the API token may not use this database",
        7404 => ": no such database in this account",
        _ => "",
    }
}

// ---------------------------------------------------------------------------
// Cloudflare API wire types
// ---------------------------------------------------------------------------

/// The body of a `/raw` request: one statement with its parameters, or
/// several without any, joined into one script.
#[derive(Debug, Serialize, PartialEq, Eq)]
struct QueryBody {
    sql: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    params: Vec<String>,
}

impl QueryBody {
    fn of(statements: &[Statement]) -> Attempted<Self> {
        match statements {
            [] => Err(Failure::permanent("no statements to send to D1")),
            [statement] => Ok(Self {
                sql: statement.sql.clone(),
                params: statement
                    .arguments
                    .iter()
                    .map(parameter)
                    .collect::<Attempted<_>>()?,
            }),
            several => {
                if several
                    .iter()
                    .any(|statement| !statement.arguments.is_empty())
                {
                    // D1 refuses parameters in a request of several
                    // statements; every such request here binds nothing.
                    return Err(Failure::permanent(
                        "D1 cannot bind parameters in a request of several statements",
                    ));
                }
                Ok(Self {
                    sql: several
                        .iter()
                        .map(|statement| statement.sql.as_str())
                        .collect::<Vec<_>>()
                        .join(";\n"),
                    params: Vec::new(),
                })
            }
        }
    }
}

/// How D1 is sent a bound value: always as text ([`DIALECT`] decodes blobs
/// and turns empty values into null).
fn parameter(value: &SqlValue) -> Attempted<String> {
    match value {
        SqlValue::Integer(value) => Ok(value.to_string()),
        SqlValue::Text(value) | SqlValue::OptionalText(Some(value)) => Ok(value.clone()),
        SqlValue::OptionalText(None) => Ok(String::new()),
        SqlValue::Blob(bytes) => Ok(hex::encode(bytes)),
        SqlValue::Null | SqlValue::Float(_) | SqlValue::Unreadable => Err(Failure::permanent(
            "D1 cannot bind a null or floating point parameter",
        )),
    }
}

/// The envelope of every Cloudflare API response.
#[derive(Debug, Deserialize)]
struct Envelope {
    #[serde(default)]
    success: bool,
    #[serde(default, deserialize_with = "null_as_empty")]
    errors: Vec<ApiError>,
    #[serde(default, deserialize_with = "null_as_empty")]
    result: Vec<QueryResult>,
}

#[derive(Debug, Deserialize)]
struct ApiError {
    #[serde(default)]
    code: Option<i64>,
    #[serde(default)]
    message: String,
}

#[derive(Debug, Deserialize)]
struct QueryResult {
    #[serde(default)]
    results: Option<Rows>,
    #[serde(default = "succeeded")]
    success: bool,
    #[serde(default)]
    meta: Option<Meta>,
}

const fn succeeded() -> bool {
    true
}

impl QueryResult {
    fn into_result(self) -> ExecuteResult {
        ExecuteResult {
            rows: self
                .results
                .map(|rows| {
                    rows.rows
                        .iter()
                        .map(|row| row.iter().map(value).collect())
                        .collect()
                })
                .unwrap_or_default(),
            affected_row_count: self.meta.map_or(0, |meta| meta.changes),
        }
    }
}

#[derive(Debug, Deserialize)]
struct Rows {
    #[serde(default)]
    rows: Vec<Vec<serde_json::Value>>,
}

#[derive(Debug, Deserialize)]
struct Meta {
    #[serde(default)]
    changes: u64,
}

/// A `null` where a list belongs reads as an empty list.
fn null_as_empty<'input, Input, Item>(
    deserializer: Input,
) -> std::result::Result<Vec<Item>, Input::Error>
where
    Input: serde::Deserializer<'input>,
    Item: Deserialize<'input>,
{
    Ok(Option::<Vec<Item>>::deserialize(deserializer)?.unwrap_or_default())
}

/// The value a row cell holds. D1 sends integers as JSON numbers, so one
/// beyond 2^53 (or with a fraction) is not read as an integer: it may have
/// lost precision. A blob arrives as a list of byte values.
fn value(cell: &serde_json::Value) -> SqlValue {
    match cell {
        serde_json::Value::Null => SqlValue::Null,
        serde_json::Value::String(text) => SqlValue::Text(text.clone()),
        serde_json::Value::Number(number) => match number.as_i64() {
            Some(integer) if integer.unsigned_abs() <= LARGEST_EXACT_INTEGER => {
                SqlValue::Integer(integer)
            }
            _ => number
                .as_f64()
                .map_or(SqlValue::Unreadable, SqlValue::Float),
        },
        serde_json::Value::Array(items) => items
            .iter()
            .map(|item| item.as_u64().and_then(|byte| u8::try_from(byte).ok()))
            .collect::<Option<Vec<u8>>>()
            .map_or(SqlValue::Unreadable, SqlValue::Blob),
        serde_json::Value::Bool(_) | serde_json::Value::Object(_) => SqlValue::Unreadable,
    }
}

#[cfg(test)]
mod tests;
