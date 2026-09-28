//! Turso (libSQL) state store over the Hrana HTTP protocol.
//!
//! Talks to `POST {url}/v2/pipeline` with bearer-token authentication, so it works
//! against Turso Cloud databases and self-hosted `sqld` alike without a
//! native libSQL dependency.
//!
//! The schema is versioned: `cuenv_infrastructure_schema` records the newest
//! migration applied, and [`StateStore::migrate`] applies only newer ones, each
//! in its own transaction. Every operation refuses a database whose schema is
//! newer than this build knows. Reads ([`StateStore::list`] and
//! [`StateStore::current_lock`]) never migrate: on a database without cuenv's
//! tables they return nothing, so a read-only token can plan and inspect
//! state. Taking the lock requires the current schema.
//!
//! Transient failures (connection errors, timeouts, HTTP 429 and 5xx,
//! `SQLITE_BUSY`) are retried with exponential backoff; every statement issued
//! here is safe to repeat.
//!
//! Transport: redirects are never followed, response bodies are read only up
//! to a fixed size, and a plaintext loopback URL (a local `sqld`) is always
//! contacted directly, never through an `HTTP_PROXY` that would see the token.

use std::fmt;
use std::future::Future;
use std::iter;
use std::net::IpAddr;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use serde::{Deserialize, Serialize};

use super::{LockInformation, ManagedResource, ResourceAddress, StateLock, StateStore};
use crate::error::{InfrastructureError, Result};
use crate::tenant::TenantKey;

/// One schema migration: the statements that move the database to `version`.
struct Migration {
    version: i64,
    statements: &'static [&'static str],
}

/// Ordered schema migrations. Never edit an existing entry; append a new one.
const MIGRATIONS: &[Migration] = &[
    // Version 1 is the original schema. `IF NOT EXISTS` adopts databases that
    // were created before the schema was versioned.
    Migration {
        version: 1,
        statements: &[
            "CREATE TABLE IF NOT EXISTS cuenv_infrastructure_resources (
                module_path TEXT NOT NULL,
                project TEXT NOT NULL,
                resource_type TEXT NOT NULL,
                resource_name TEXT NOT NULL,
                provider TEXT NOT NULL,
                provider_source TEXT NOT NULL,
                schema_version INTEGER NOT NULL,
                state_json TEXT NOT NULL,
                private BLOB,
                dependencies_json TEXT NOT NULL DEFAULT '[]',
                serial INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY (module_path, project, resource_type, resource_name)
            ) WITHOUT ROWID",
            "CREATE TABLE IF NOT EXISTS cuenv_infrastructure_locks (
                module_path TEXT NOT NULL,
                project TEXT NOT NULL,
                lock_identifier TEXT NOT NULL,
                holder TEXT NOT NULL,
                acquired_at TEXT NOT NULL,
                PRIMARY KEY (module_path, project)
            ) WITHOUT ROWID",
        ],
    },
    Migration {
        version: 2,
        statements: &[
            "ALTER TABLE cuenv_infrastructure_resources \
             ADD COLUMN tainted INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE cuenv_infrastructure_resources ADD COLUMN identity_json TEXT",
        ],
    },
];

/// Newest schema version this build knows.
const LATEST_SCHEMA_VERSION: i64 = MIGRATIONS[MIGRATIONS.len() - 1].version;

/// First schema version with the `tainted` and `identity_json` columns.
const TAINT_AND_IDENTITY_SCHEMA_VERSION: i64 = 2;

const SCHEMA_TABLE: &str = "cuenv_infrastructure_schema";
const RESOURCES_TABLE: &str = "cuenv_infrastructure_resources";
const LOCKS_TABLE: &str = "cuenv_infrastructure_locks";

const CREATE_SCHEMA_TABLE: &str =
    "CREATE TABLE IF NOT EXISTS cuenv_infrastructure_schema (version INTEGER NOT NULL)";

const SELECT_SCHEMA_VERSION: &str =
    "SELECT COALESCE(MAX(version), 0) FROM cuenv_infrastructure_schema";

const SELECT_RESOURCES: &str = "SELECT resource_type, resource_name, provider, provider_source, \
     schema_version, state_json, private, dependencies_json, tainted, identity_json \
     FROM cuenv_infrastructure_resources WHERE module_path = ? AND project = ? \
     ORDER BY resource_type, resource_name";

/// [`SELECT_RESOURCES`] for schemas older than
/// [`TAINT_AND_IDENTITY_SCHEMA_VERSION`], which a read does not migrate.
const SELECT_RESOURCES_WITHOUT_TAINT_AND_IDENTITY: &str = "SELECT resource_type, resource_name, provider, provider_source, \
     schema_version, state_json, private, dependencies_json, 0, NULL \
     FROM cuenv_infrastructure_resources WHERE module_path = ? AND project = ? \
     ORDER BY resource_type, resource_name";

/// Longest HTTP error body quoted in an error message, in bytes.
const MAXIMUM_ERROR_BODY_BYTES: usize = 4096;

/// Largest successful Turso response read, in bytes (64 MiB).
const MAXIMUM_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

/// Per-request timeout for Turso calls.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

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

/// How transient failures are retried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RetryPolicy {
    /// Retries after the first attempt.
    retries: u32,
    /// Delay before the first retry; doubled before each further retry.
    initial_delay: Duration,
}

impl RetryPolicy {
    const DEFAULT: Self = Self {
        retries: 3,
        initial_delay: Duration::from_millis(200),
    };

    /// Delay before retry number `retry` (zero based): 200 ms, 400 ms, 800 ms.
    fn delay(self, retry: u32) -> Duration {
        self.initial_delay
            .saturating_mul(2_u32.saturating_pow(retry))
    }
}

/// [`StateStore`] backed by a remote Turso database.
#[derive(Clone)]
pub struct TursoStateStore {
    client: reqwest::Client,
    pipeline_url: reqwest::Url,
    authentication_token: Option<String>,
    retry_policy: RetryPolicy,
    /// Largest successful response body read, in bytes.
    maximum_response_bytes: usize,
}

impl fmt::Debug for TursoStateStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TursoStateStore")
            .field("pipeline_url", &self.pipeline_url.as_str())
            .field(
                "authentication_token",
                &self.authentication_token.as_ref().map(|_| "<redacted>"),
            )
            .field("retry_policy", &self.retry_policy)
            .field("maximum_response_bytes", &self.maximum_response_bytes)
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

    fn with_timeout(configuration: TursoConfiguration, timeout: Duration) -> Result<Self> {
        let pipeline_url = pipeline_url(&configuration.url)?;
        crate::ensure_rustls_cryptography_provider();
        let builder = reqwest::Client::builder()
            .timeout(timeout)
            // A redirect could carry the request, and its token, elsewhere.
            .redirect(reqwest::redirect::Policy::none());
        // `pipeline_url` produces `http` only for a loopback host: talk to it
        // directly, never through a proxy that would see the token in clear.
        let builder = if pipeline_url.scheme() == "http" {
            builder.no_proxy()
        } else {
            builder
        };
        let client = builder.build().map_err(|error| {
            InfrastructureError::state(format!(
                "failed to build HTTP client: {}",
                describe_transport_error(&error)
            ))
        })?;
        Ok(Self {
            client,
            pipeline_url,
            authentication_token: configuration
                .authentication_token
                .filter(|token| !token.is_empty()),
            retry_policy: RetryPolicy::DEFAULT,
            maximum_response_bytes: MAXIMUM_RESPONSE_BYTES,
        })
    }

    /// Send one pipeline request, without retrying.
    async fn send(&self, body: &PipelineBody<'_>) -> Attempted<PipelineResponse> {
        let mut request = self.client.post(self.pipeline_url.clone()).json(body);
        if let Some(token) = &self.authentication_token {
            request = request.bearer_auth(token);
        }
        let mut response = request
            .send()
            .await
            .map_err(|error| transport_failure(&error))?;
        let status = response.status();
        if !status.is_success() {
            let total_bytes = response.content_length();
            let body = read_body_prefix(&mut response, MAXIMUM_ERROR_BODY_BYTES).await;
            return Err(Failure {
                kind: if status_is_transient(status) {
                    FailureKind::Transient
                } else {
                    FailureKind::Permanent
                },
                message: format!(
                    "Turso returned HTTP {status}: {}",
                    describe_error_body(&body, total_bytes)
                ),
            });
        }
        let bytes = read_whole_body(&mut response, self.maximum_response_bytes).await?;
        serde_json::from_slice(&bytes).map_err(|error| Failure {
            kind: FailureKind::Permanent,
            message: format!("invalid Turso response: {error}"),
        })
    }

    /// Execute statements in one pipeline request, without retrying.
    ///
    /// Statements run in order, each in its own implicit transaction.
    async fn pipeline_once(&self, statements: &[Statement]) -> Attempted<Vec<ExecuteResult>> {
        let count = statements.len();
        let requests = statements
            .iter()
            .map(|statement| PipelineRequest::Execute { statement })
            .chain(iter::once(PipelineRequest::Close))
            .collect();
        let response = self
            .send(&PipelineBody {
                baton: None,
                requests,
            })
            .await?;

        let mut results = Vec::with_capacity(count);
        for entry in response.results.into_iter().take(count) {
            match entry {
                PipelineResult::Ok {
                    response: StreamResponse::Execute { result },
                } => results.push(result),
                PipelineResult::Ok { .. } => {
                    return Err(Failure::permanent("unexpected Turso response type"));
                }
                PipelineResult::Error { error } => return Err(statement_failure(&error)),
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

    /// Execute a conditional batch in one pipeline request, without retrying.
    async fn batch_once(&self, steps: Vec<BatchStep<'_>>) -> Attempted<BatchResult> {
        let response = self
            .send(&PipelineBody {
                baton: None,
                requests: vec![
                    PipelineRequest::Batch {
                        batch: Batch { steps },
                    },
                    PipelineRequest::Close,
                ],
            })
            .await?;
        match response.results.into_iter().next() {
            Some(PipelineResult::Ok {
                response: StreamResponse::Batch { result },
            }) => Ok(result),
            Some(PipelineResult::Error { error }) => Err(statement_failure(&error)),
            Some(PipelineResult::Ok { .. }) | None => {
                Err(Failure::permanent("unexpected Turso response to a batch"))
            }
        }
    }

    /// Run `attempt` until it succeeds, fails permanently, or retries run out.
    async fn retrying<Output, Attempt>(
        &self,
        mut attempt: impl FnMut() -> Attempt,
    ) -> Attempted<Output>
    where
        Attempt: Future<Output = Attempted<Output>>,
    {
        let mut retry = 0;
        loop {
            match attempt().await {
                Ok(output) => return Ok(output),
                Err(failure) if failure.is_transient() && retry < self.retry_policy.retries => {
                    let delay = self.retry_policy.delay(retry);
                    tracing::warn!(
                        attempt = retry + 1,
                        delay_milliseconds = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
                        error = %self.redact(&failure.message),
                        "transient Turso failure; retrying"
                    );
                    tokio::time::sleep(delay).await;
                    retry += 1;
                }
                Err(failure) if failure.is_transient() && retry > 0 => {
                    return Err(Failure {
                        kind: failure.kind,
                        message: format!(
                            "{} (gave up after {} attempts)",
                            failure.message,
                            retry + 1
                        ),
                    });
                }
                Err(failure) => return Err(failure),
            }
        }
    }

    /// Execute statements with retries.
    async fn pipeline(&self, statements: &[Statement]) -> Result<Vec<ExecuteResult>> {
        self.retrying(move || self.pipeline_once(statements))
            .await
            .map_err(|failure| self.error(&failure))
    }

    /// Execute one statement with retries.
    async fn execute(&self, statement: Statement) -> Result<ExecuteResult> {
        self.pipeline(std::slice::from_ref(&statement))
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| InfrastructureError::state("Turso returned no result"))
    }

    /// Turn a failure into an error, never leaking the authentication token.
    fn error(&self, failure: &Failure) -> InfrastructureError {
        InfrastructureError::state(self.redact(&failure.message))
    }

    fn redact(&self, message: &str) -> String {
        match &self.authentication_token {
            Some(token) => message.replace(token.as_str(), "<redacted>"),
            None => message.to_string(),
        }
    }

    /// The newest applied schema version; 0 for a database never migrated.
    async fn schema_version(&self) -> Result<i64> {
        let results = self
            .pipeline(&[
                Statement::new(CREATE_SCHEMA_TABLE, Vec::new()),
                Statement::new(SELECT_SCHEMA_VERSION, Vec::new()),
            ])
            .await?;
        results
            .get(1)
            .and_then(|result| result.rows.first())
            .and_then(|row| row.first())
            .and_then(HranaValue::as_integer)
            .ok_or_else(|| InfrastructureError::state("Turso returned no schema version"))
    }

    /// Inspect the schema without creating or changing anything.
    ///
    /// Fails closed on a schema newer than this build knows, so an older
    /// cuenv never reads or writes rows whose meaning may have changed.
    async fn stored_schema(&self) -> Result<StoredSchema> {
        let tables = self
            .execute(Statement::new(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name IN (?, ?, ?)",
                [SCHEMA_TABLE, RESOURCES_TABLE, LOCKS_TABLE]
                    .into_iter()
                    .map(HranaValue::text)
                    .collect(),
            ))
            .await?;
        let present = |table: &str| {
            tables
                .rows
                .iter()
                .any(|row| row.first().and_then(HranaValue::as_text) == Some(table))
        };
        let version = if present(SCHEMA_TABLE) {
            self.execute(Statement::new(SELECT_SCHEMA_VERSION, Vec::new()))
                .await?
                .rows
                .first()
                .and_then(|row| row.first())
                .and_then(HranaValue::as_integer)
                .ok_or_else(|| InfrastructureError::state("Turso returned no schema version"))?
        } else {
            0
        };
        if version > LATEST_SCHEMA_VERSION {
            return Err(newer_schema(version));
        }
        Ok(StoredSchema {
            version,
            resources_table: present(RESOURCES_TABLE),
            locks_table: present(LOCKS_TABLE),
        })
    }

    /// Apply one migration and record its version, atomically.
    ///
    /// Returns [`MigrationOutcome::ColumnAlreadyExists`] when another process
    /// applied the same migration concurrently.
    async fn apply_migration(&self, migration: &Migration) -> Result<MigrationOutcome> {
        let version_arguments = || vec![HranaValue::integer(migration.version)];
        let statements: Vec<Statement> = iter::once(Statement::new("BEGIN IMMEDIATE", Vec::new()))
            .chain(
                migration
                    .statements
                    .iter()
                    .map(|sql| Statement::new(*sql, Vec::new())),
            )
            // The recorded version only ever moves forward, even if a slower
            // migrator finishes an older step after a faster one.
            .chain([
                Statement::new(
                    "DELETE FROM cuenv_infrastructure_schema WHERE version < ?",
                    version_arguments(),
                ),
                Statement::new(
                    "INSERT INTO cuenv_infrastructure_schema (version) SELECT ?1 \
                     WHERE NOT EXISTS (SELECT 1 FROM cuenv_infrastructure_schema WHERE version >= ?1)",
                    version_arguments(),
                ),
                Statement::new("COMMIT", Vec::new()),
                Statement::new("ROLLBACK", Vec::new()),
            ])
            .collect();
        let commit_index = statements.len() - 2;
        let statements = statements.as_slice();
        self.retrying(move || async move {
            let result = self
                .batch_once(transaction_steps(statements, commit_index))
                .await?;
            migration_outcome(&result, commit_index)
        })
        .await
        .map_err(|failure| self.error(&failure))
    }

    /// Read the tenant's lock row, with retries.
    async fn read_lock(&self, tenant: &TenantKey) -> Result<Option<LockInformation>> {
        let result = self
            .execute(Statement::new(
                "SELECT lock_identifier, holder, acquired_at FROM cuenv_infrastructure_locks \
                 WHERE module_path = ? AND project = ?",
                tenant_arguments(tenant),
            ))
            .await?;
        Ok(result.rows.first().map(|row| {
            let field = |index: usize| {
                row.get(index)
                    .and_then(HranaValue::as_text)
                    .unwrap_or("unknown")
                    .to_string()
            };
            LockInformation {
                lock_identifier: field(0),
                holder: field(1),
                acquired_at: field(2),
            }
        }))
    }
}

/// Convert a database URL into the Hrana v2 pipeline endpoint.
///
/// Plaintext schemes (`http://`, `ws://`) are accepted only for loopback hosts,
/// because the bearer token would otherwise cross the network unencrypted.
/// Error messages never quote the full URL, which may carry secrets.
fn pipeline_url(url: &str) -> Result<reqwest::Url> {
    let url = url.trim();
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
            return Err(InfrastructureError::configuration(format!(
                "unsupported Turso URL scheme '{scheme}://'; expected libsql://, https:// or \
                 wss:// (http:// and ws:// only for a loopback host)"
            )));
        }
    };
    let mut parsed =
        reqwest::Url::parse(&format!("{http_scheme}://{remainder}")).map_err(|error| {
            InfrastructureError::configuration(format!("invalid Turso URL: {error}"))
        })?;
    let host = parsed
        .host_str()
        .filter(|host| !host.is_empty())
        .ok_or_else(|| InfrastructureError::configuration("invalid Turso URL: missing host"))?
        .to_string();
    if !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(InfrastructureError::configuration(format!(
            "Turso URL for host '{host}' must not contain credentials, a query or a fragment; \
             pass the token as the authentication token instead"
        )));
    }
    if transport == Transport::Plaintext && !is_loopback_host(&host) {
        return Err(InfrastructureError::configuration(format!(
            "Turso URL uses plaintext {scheme}:// for non-loopback host '{host}', which would \
             send the authentication token unencrypted; non-loopback URLs must use libsql://, \
             https:// or wss://"
        )));
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

/// `localhost`, `127.0.0.0/8`, `::1`, or an IPv4-mapped loopback address.
fn is_loopback_host(host: &str) -> bool {
    let unbracketed = host
        .strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(host);
    match unbracketed.parse::<IpAddr>() {
        Ok(IpAddr::V4(address)) => address.is_loopback(),
        Ok(IpAddr::V6(address)) => {
            address.is_loopback()
                || address
                    .to_ipv4_mapped()
                    .is_some_and(|mapped| mapped.is_loopback())
        }
        Err(_) => unbracketed.eq_ignore_ascii_case("localhost"),
    }
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn tenant_arguments(tenant: &TenantKey) -> Vec<HranaValue> {
    vec![
        HranaValue::text(tenant.module_path()),
        HranaValue::text(tenant.project()),
    ]
}

#[async_trait]
impl StateStore for TursoStateStore {
    #[tracing::instrument(skip_all)]
    async fn migrate(&self) -> Result<()> {
        let current = self.schema_version().await?;
        if current > LATEST_SCHEMA_VERSION {
            return Err(newer_schema(current));
        }
        for migration in MIGRATIONS
            .iter()
            .filter(|migration| migration.version > current)
        {
            match self.apply_migration(migration).await? {
                MigrationOutcome::Applied => {
                    tracing::info!(
                        version = migration.version,
                        "applied state schema migration"
                    );
                }
                MigrationOutcome::ColumnAlreadyExists => {
                    // Another process applied this migration first; its
                    // transaction also recorded the version.
                    let recorded = self.schema_version().await?;
                    if recorded < migration.version {
                        return Err(InfrastructureError::state(format!(
                            "state schema migration {} found its columns already present but \
                             the recorded schema version is {recorded}",
                            migration.version
                        )));
                    }
                    tracing::debug!(
                        version = migration.version,
                        "state schema migration applied concurrently by another process"
                    );
                }
            }
        }
        Ok(())
    }

    #[tracing::instrument(skip_all, fields(tenant = %tenant))]
    async fn list(&self, tenant: &TenantKey) -> Result<Vec<ManagedResource>> {
        let schema = self.stored_schema().await?;
        if !schema.resources_table {
            // Never migrated: nothing has been recorded yet.
            return Ok(Vec::new());
        }
        // Version 0 is a table created before the schema was versioned, which
        // has the version 1 layout.
        let query = if schema.version >= TAINT_AND_IDENTITY_SCHEMA_VERSION {
            SELECT_RESOURCES
        } else {
            SELECT_RESOURCES_WITHOUT_TAINT_AND_IDENTITY
        };
        let result = self
            .execute(Statement::new(query, tenant_arguments(tenant)))
            .await?;
        result.rows.iter().map(|row| row_to_resource(row)).collect()
    }

    #[tracing::instrument(skip_all, fields(tenant = %tenant, address = %resource.address))]
    async fn put(
        &self,
        tenant: &TenantKey,
        lock: &StateLock,
        resource: &ManagedResource,
    ) -> Result<()> {
        let state_json = serde_json::to_string(&resource.state)
            .map_err(|error| InfrastructureError::state(format!("serialize state: {error}")))?;
        let dependencies = serde_json::to_string(&resource.dependencies).map_err(|error| {
            InfrastructureError::state(format!("serialize dependencies: {error}"))
        })?;
        let identity_json = resource
            .identity
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|error| InfrastructureError::state(format!("serialize identity: {error}")))?;
        let timestamp = now();
        // The row is written only while this run still holds the lock; the
        // check and the write are one statement, so they are atomic.
        let written = self
            .execute(Statement::new(
                "INSERT INTO cuenv_infrastructure_resources (module_path, project, resource_type, \
                 resource_name, provider, provider_source, schema_version, state_json, private, \
                 dependencies_json, tainted, identity_json, serial, created_at, updated_at) \
                 SELECT ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ? \
                 WHERE EXISTS (SELECT 1 FROM cuenv_infrastructure_locks \
                 WHERE module_path = ? AND project = ? AND lock_identifier = ?) \
                 ON CONFLICT (module_path, project, resource_type, resource_name) DO UPDATE SET \
                 provider = excluded.provider, provider_source = excluded.provider_source, \
                 schema_version = excluded.schema_version, state_json = excluded.state_json, \
                 private = excluded.private, dependencies_json = excluded.dependencies_json, \
                 tainted = excluded.tainted, identity_json = excluded.identity_json, \
                 serial = cuenv_infrastructure_resources.serial + 1, updated_at = excluded.updated_at",
                vec![
                    HranaValue::text(tenant.module_path()),
                    HranaValue::text(tenant.project()),
                    HranaValue::text(&resource.address.resource_type),
                    HranaValue::text(&resource.address.name),
                    HranaValue::text(&resource.provider),
                    HranaValue::text(&resource.provider_source),
                    HranaValue::integer(resource.schema_version),
                    HranaValue::text(&state_json),
                    HranaValue::blob(&resource.private),
                    HranaValue::text(&dependencies),
                    HranaValue::integer(i64::from(resource.tainted)),
                    HranaValue::optional_text(identity_json.as_deref()),
                    HranaValue::text(&timestamp),
                    HranaValue::text(&timestamp),
                    HranaValue::text(tenant.module_path()),
                    HranaValue::text(tenant.project()),
                    HranaValue::text(&lock.lock_identifier),
                ],
            ))
            .await?;
        if written.affected_row_count == 0 {
            return Err(lock_lost(tenant, lock));
        }
        Ok(())
    }

    #[tracing::instrument(skip_all, fields(tenant = %tenant, address = %address))]
    async fn delete(
        &self,
        tenant: &TenantKey,
        lock: &StateLock,
        address: &ResourceAddress,
    ) -> Result<()> {
        let deleted = self
            .execute(Statement::new(
                "DELETE FROM cuenv_infrastructure_resources WHERE module_path = ? AND project = ? \
                 AND resource_type = ? AND resource_name = ? \
                 AND EXISTS (SELECT 1 FROM cuenv_infrastructure_locks \
                 WHERE module_path = ? AND project = ? AND lock_identifier = ?)",
                vec![
                    HranaValue::text(tenant.module_path()),
                    HranaValue::text(tenant.project()),
                    HranaValue::text(&address.resource_type),
                    HranaValue::text(&address.name),
                    HranaValue::text(tenant.module_path()),
                    HranaValue::text(tenant.project()),
                    HranaValue::text(&lock.lock_identifier),
                ],
            ))
            .await?;
        // Nothing deleted means the row was already gone (possibly by an
        // earlier attempt of this same call) or the lock was lost; only the
        // second is an error.
        if deleted.affected_row_count == 0 {
            let held = self
                .read_lock(tenant)
                .await?
                .is_some_and(|information| information.lock_identifier == lock.lock_identifier);
            if !held {
                return Err(lock_lost(tenant, lock));
            }
        }
        Ok(())
    }

    #[tracing::instrument(skip_all, fields(tenant = %tenant, holder = %holder))]
    async fn lock(&self, tenant: &TenantKey, holder: &str) -> Result<StateLock> {
        // Every write needs the lock, so this is where writes fail closed on
        // a schema this build has not migrated to.
        let schema = self.stored_schema().await?;
        if schema.version != LATEST_SCHEMA_VERSION || !schema.resources_table || !schema.locks_table
        {
            return Err(InfrastructureError::state(format!(
                "Turso state schema is at version {} but this cuenv writes version \
                 {LATEST_SCHEMA_VERSION}; migrate the state store before taking the lock",
                schema.version
            )));
        }
        let lock_identifier = uuid::Uuid::new_v4().to_string();
        let insert = Statement::new(
            "INSERT INTO cuenv_infrastructure_locks (module_path, project, lock_identifier, holder, acquired_at) \
             VALUES (?, ?, ?, ?, ?) ON CONFLICT (module_path, project) DO NOTHING",
            vec![
                HranaValue::text(tenant.module_path()),
                HranaValue::text(tenant.project()),
                HranaValue::text(&lock_identifier),
                HranaValue::text(holder),
                HranaValue::text(&now()),
            ],
        );
        let ours = |information: &LockInformation| information.lock_identifier == lock_identifier;
        let acquired = || StateLock {
            lock_identifier: lock_identifier.clone(),
        };

        let mut retry = 0;
        loop {
            let last_attempt = retry >= self.retry_policy.retries;
            match self.pipeline_once(std::slice::from_ref(&insert)).await {
                Ok(results)
                    if results
                        .first()
                        .is_some_and(|result| result.affected_row_count == 1) =>
                {
                    return Ok(acquired());
                }
                Ok(_) => match self.read_lock(tenant).await? {
                    // An earlier attempt whose response was lost did commit.
                    Some(information) if ours(&information) => return Ok(acquired()),
                    Some(information) => {
                        return Err(locked(tenant, information));
                    }
                    // Released between the insert and the read: try again.
                    None if !last_attempt => {}
                    None => {
                        return Err(InfrastructureError::state(format!(
                            "could not acquire the state lock for {tenant}: it kept changing hands"
                        )));
                    }
                },
                Err(failure) if failure.is_transient() => {
                    // The insert may have committed even though the response
                    // was lost; check before retrying or failing.
                    match self.read_lock(tenant).await {
                        Ok(Some(information)) if ours(&information) => return Ok(acquired()),
                        Ok(Some(information)) => {
                            return Err(locked(tenant, information));
                        }
                        Ok(None) | Err(_) if !last_attempt => {}
                        Ok(None) => return Err(self.error(&failure)),
                        Err(read_error) => {
                            return Err(InfrastructureError::state(format!(
                                "{}; the state lock for {tenant} may have been acquired as \
                                 {lock_identifier} (checking failed: {read_error}); if so, \
                                 release it with `cuenv infrastructure unlock {lock_identifier}`",
                                self.redact(&failure.message)
                            )));
                        }
                    }
                    let delay = self.retry_policy.delay(retry);
                    tracing::warn!(
                        attempt = retry + 1,
                        error = %self.redact(&failure.message),
                        "transient Turso failure acquiring the state lock; retrying"
                    );
                    tokio::time::sleep(delay).await;
                }
                Err(failure) => return Err(self.error(&failure)),
            }
            retry += 1;
        }
    }

    #[tracing::instrument(skip_all, fields(tenant = %tenant))]
    async fn unlock(&self, tenant: &TenantKey, lock: &StateLock) -> Result<()> {
        self.execute(Statement::new(
            "DELETE FROM cuenv_infrastructure_locks WHERE module_path = ? AND project = ? AND lock_identifier = ?",
            vec![
                HranaValue::text(tenant.module_path()),
                HranaValue::text(tenant.project()),
                HranaValue::text(&lock.lock_identifier),
            ],
        ))
        .await
        .map(|_| ())
    }

    #[tracing::instrument(skip_all, fields(tenant = %tenant))]
    async fn current_lock(&self, tenant: &TenantKey) -> Result<Option<LockInformation>> {
        if !self.stored_schema().await?.locks_table {
            // Never migrated: nobody can have taken the lock.
            return Ok(None);
        }
        self.read_lock(tenant).await
    }

    #[tracing::instrument(skip_all, fields(tenant = %tenant, lock_identifier = %lock_identifier))]
    async fn force_unlock(&self, tenant: &TenantKey, lock_identifier: &str) -> Result<bool> {
        if !self.stored_schema().await?.locks_table {
            return Ok(false);
        }
        let released = self
            .execute(Statement::new(
                "DELETE FROM cuenv_infrastructure_locks \
                 WHERE module_path = ? AND project = ? AND lock_identifier = ?",
                vec![
                    HranaValue::text(tenant.module_path()),
                    HranaValue::text(tenant.project()),
                    HranaValue::text(lock_identifier),
                ],
            ))
            .await?;
        Ok(released.affected_row_count > 0)
    }
}

/// What a read-only inspection of the database found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StoredSchema {
    /// Recorded schema version; 0 when nothing is recorded.
    version: i64,
    /// Whether `cuenv_infrastructure_resources` exists.
    resources_table: bool,
    /// Whether `cuenv_infrastructure_locks` exists.
    locks_table: bool,
}

fn newer_schema(version: i64) -> InfrastructureError {
    InfrastructureError::state(format!(
        "Turso state schema version {version} is newer than this cuenv supports \
         ({LATEST_SCHEMA_VERSION}); upgrade cuenv"
    ))
}

fn locked(tenant: &TenantKey, information: LockInformation) -> InfrastructureError {
    InfrastructureError::Locked {
        tenant: tenant.to_string(),
        lock_identifier: information.lock_identifier,
        holder: information.holder,
        acquired_at: information.acquired_at,
    }
}

fn lock_lost(tenant: &TenantKey, lock: &StateLock) -> InfrastructureError {
    InfrastructureError::LockLost {
        tenant: tenant.to_string(),
        lock_identifier: lock.lock_identifier.clone(),
    }
}

fn row_to_resource(row: &[HranaValue]) -> Result<ManagedResource> {
    let text = |index: usize, name: &str| {
        row.get(index)
            .and_then(HranaValue::as_text)
            .map(str::to_string)
            .ok_or_else(|| InfrastructureError::state(format!("state row missing {name}")))
    };
    let integer = |index: usize, name: &str| {
        row.get(index)
            .and_then(HranaValue::as_integer)
            .ok_or_else(|| InfrastructureError::state(format!("state row missing {name}")))
    };
    let state_json = text(5, "state_json")?;
    let dependencies_json = text(7, "dependencies_json")?;
    let identity = row
        .get(9)
        .and_then(HranaValue::as_text)
        .map(serde_json::from_str)
        .transpose()
        .map_err(|error| InfrastructureError::state(format!("corrupt identity_json: {error}")))?;
    Ok(ManagedResource {
        address: ResourceAddress::new(text(0, "resource_type")?, text(1, "resource_name")?),
        provider: text(2, "provider")?,
        provider_source: text(3, "provider_source")?,
        schema_version: integer(4, "schema_version")?,
        state: serde_json::from_str(&state_json)
            .map_err(|error| InfrastructureError::state(format!("corrupt state_json: {error}")))?,
        private: row
            .get(6)
            .map(HranaValue::as_blob)
            .transpose()?
            .flatten()
            .unwrap_or_default(),
        dependencies: serde_json::from_str(&dependencies_json).map_err(|error| {
            InfrastructureError::state(format!("corrupt dependencies_json: {error}"))
        })?,
        tainted: integer(8, "tainted")? != 0,
        identity,
    })
}

// ---------------------------------------------------------------------------
// Failure classification
// ---------------------------------------------------------------------------

type Attempted<Output> = std::result::Result<Output, Failure>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureKind {
    /// Worth retrying: the request may not have reached the database, or the
    /// database was briefly unavailable. The request may also have committed.
    Transient,
    /// The database rejected a statement.
    Statement,
    /// Retrying cannot help.
    Permanent,
}

#[derive(Debug)]
struct Failure {
    kind: FailureKind,
    message: String,
}

impl Failure {
    fn permanent(message: impl Into<String>) -> Self {
        Self {
            kind: FailureKind::Permanent,
            message: message.into(),
        }
    }

    fn is_transient(&self) -> bool {
        self.kind == FailureKind::Transient
    }
}

/// HTTP statuses worth retrying: 408, 429 and every 5xx.
fn status_is_transient(status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::REQUEST_TIMEOUT
        || status == reqwest::StatusCode::TOO_MANY_REQUESTS
        || status.is_server_error()
}

/// Classify a failure to send a request or read its response.
///
/// Connection failures, timeouts and interrupted bodies are transient; only
/// request construction and redirect policy failures are permanent.
fn transport_failure(error: &reqwest::Error) -> Failure {
    Failure {
        kind: if error.is_builder() || error.is_redirect() {
            FailureKind::Permanent
        } else {
            FailureKind::Transient
        },
        message: format!("Turso request failed: {}", describe_transport_error(error)),
    }
}

/// Render an HTTP client error with its whole source chain, saying explicitly
/// when it was a timeout.
fn describe_transport_error(error: &reqwest::Error) -> String {
    let mut messages: Vec<String> = vec![error.to_string()];
    for source in iter::successors(std::error::Error::source(error), |source| source.source()) {
        let message = source.to_string();
        if messages
            .last()
            .is_none_or(|previous| !previous.contains(&message))
        {
            messages.push(message);
        }
    }
    let chain = messages.join(": ");
    if error.is_timeout() {
        format!("timed out: {chain}")
    } else {
        chain
    }
}

fn statement_failure(error: &HranaError) -> Failure {
    let busy = error
        .code
        .as_deref()
        .is_some_and(|code| code == "SQLITE_BUSY");
    Failure {
        kind: if busy {
            FailureKind::Transient
        } else {
            FailureKind::Statement
        },
        message: format!("Turso statement failed: {error}"),
    }
}

/// The first bytes of a response body: at most `limit`, plus whether more
/// followed.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct BodyPrefix {
    bytes: Vec<u8>,
    truncated: bool,
}

/// Read at most `limit` bytes of an error response body, stopping there.
///
/// A body that fails part way keeps what arrived: it only decorates an error.
async fn read_body_prefix(response: &mut reqwest::Response, limit: usize) -> BodyPrefix {
    let mut prefix = BodyPrefix::default();
    while let Ok(Some(chunk)) = response.chunk().await {
        let room = limit - prefix.bytes.len();
        if chunk.len() > room {
            prefix.bytes.extend_from_slice(&chunk[..room]);
            prefix.truncated = true;
            break;
        }
        prefix.bytes.extend_from_slice(&chunk);
    }
    prefix
}

/// Read a successful response body, failing once it exceeds `limit` bytes.
async fn read_whole_body(response: &mut reqwest::Response, limit: usize) -> Attempted<Vec<u8>> {
    let too_large = || {
        Failure::permanent(format!(
            "Turso response exceeds the {limit}-byte limit; the tenant's state is too large to \
             read in one response"
        ))
    };
    if response
        .content_length()
        .is_some_and(|length| usize::try_from(length).map_or(true, |length| length > limit))
    {
        return Err(too_large());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| transport_failure(&error))?
    {
        if chunk.len() > limit - bytes.len() {
            return Err(too_large());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

/// Render an error body prefix for a message, cut on a character boundary.
fn describe_error_body(body: &BodyPrefix, total_bytes: Option<u64>) -> String {
    let text = String::from_utf8_lossy(&body.bytes);
    if !body.truncated {
        return text.into_owned();
    }
    // A multi-byte character cut at the limit decodes as a replacement
    // character; drop it.
    let text = text.trim_end_matches(char::REPLACEMENT_CHARACTER);
    match total_bytes {
        Some(total) => format!("{text}… (truncated, {total} bytes in total)"),
        None => format!("{text}… (truncated)"),
    }
}

// ---------------------------------------------------------------------------
// Migrations
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MigrationOutcome {
    Applied,
    /// The migration failed because its columns already exist: another
    /// process applied it concurrently.
    ColumnAlreadyExists,
}

/// Steps for a transaction batch: `statements` is `BEGIN`, the body,
/// `COMMIT` at `commit_index`, then `ROLLBACK`. Each step up to the commit
/// runs only if the previous one succeeded; the rollback runs only if the
/// commit did not.
fn transaction_steps(statements: &[Statement], commit_index: usize) -> Vec<BatchStep<'_>> {
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
            statement,
        })
        .collect()
}

fn migration_outcome(result: &BatchResult, commit_index: usize) -> Attempted<MigrationOutcome> {
    let first_error = result
        .step_errors
        .iter()
        .take(commit_index + 1)
        .flatten()
        .next();
    if let Some(error) = first_error {
        if error.message.contains("duplicate column name") {
            return Ok(MigrationOutcome::ColumnAlreadyExists);
        }
        return Err(statement_failure(error));
    }
    if result
        .step_results
        .get(commit_index)
        .is_some_and(Option::is_some)
    {
        Ok(MigrationOutcome::Applied)
    } else {
        Err(Failure::permanent(
            "Turso did not commit the schema migration transaction",
        ))
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
        statement: &'body Statement,
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
    statement: &'body Statement,
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

#[derive(Debug, Serialize)]
struct Statement {
    sql: String,
    #[serde(rename = "args")]
    arguments: Vec<HranaValue>,
    want_rows: bool,
}

impl Statement {
    fn new(sql: impl Into<String>, arguments: Vec<HranaValue>) -> Self {
        Self {
            sql: sql.into(),
            arguments,
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
        result: ExecuteResult,
    },
    Batch {
        result: BatchResult,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Default, Deserialize)]
struct ExecuteResult {
    #[serde(default)]
    rows: Vec<Vec<HranaValue>>,
    #[serde(default)]
    affected_row_count: u64,
}

#[derive(Debug, Default, Deserialize)]
struct BatchResult {
    #[serde(default)]
    step_results: Vec<Option<ExecuteResult>>,
    #[serde(default)]
    step_errors: Vec<Option<HranaError>>,
}

#[derive(Debug, Deserialize)]
struct HranaError {
    message: String,
    #[serde(default)]
    code: Option<String>,
}

impl fmt::Display for HranaError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.code {
            Some(code) => write!(formatter, "{} ({code})", self.message),
            None => formatter.write_str(&self.message),
        }
    }
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
    fn text(value: &str) -> Self {
        Self::Text {
            value: value.to_string(),
        }
    }

    fn optional_text(value: Option<&str>) -> Self {
        value.map_or(Self::Null, Self::text)
    }

    fn integer(value: i64) -> Self {
        Self::Integer {
            value: value.to_string(),
        }
    }

    fn blob(bytes: &[u8]) -> Self {
        if bytes.is_empty() {
            return Self::Null;
        }
        Self::Blob {
            base64: STANDARD_NO_PAD.encode(bytes),
        }
    }

    fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text { value } => Some(value),
            _ => None,
        }
    }

    fn as_integer(&self) -> Option<i64> {
        match self {
            Self::Integer { value } => value.parse().ok(),
            _ => None,
        }
    }

    fn as_blob(&self) -> Result<Option<Vec<u8>>> {
        match self {
            Self::Blob { base64 } => {
                let trimmed = base64.trim_end_matches('=');
                STANDARD_NO_PAD
                    .decode(trimmed)
                    .or_else(|_| STANDARD.decode(base64))
                    .map(Some)
                    .map_err(|error| {
                        InfrastructureError::state(format!("corrupt private blob: {error}"))
                    })
            }
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use serde_json::{Value, json};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    use super::*;

    fn configuration(url: &str) -> TursoConfiguration {
        TursoConfiguration {
            url: url.into(),
            authentication_token: Some("secret-token".into()),
        }
    }

    fn configuration_error(url: &str) -> String {
        match pipeline_url(url) {
            Err(InfrastructureError::Configuration(message)) => message,
            other => panic!("expected a configuration error for {url}, got {other:?}"),
        }
    }

    #[test]
    fn pipeline_url_normalizes_schemes() {
        let cases = [
            (
                "libsql://db-acme.turso.io",
                "https://db-acme.turso.io/v2/pipeline",
            ),
            (
                "LIBSQL://db-acme.turso.io/",
                "https://db-acme.turso.io/v2/pipeline",
            ),
            (
                "https://db.turso.io/prefix/",
                "https://db.turso.io/prefix/v2/pipeline",
            ),
            ("wss://db.turso.io", "https://db.turso.io/v2/pipeline"),
            ("ws://localhost:8080", "http://localhost:8080/v2/pipeline"),
            (
                "http://127.0.0.1:8080/",
                "http://127.0.0.1:8080/v2/pipeline",
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(pipeline_url(input).unwrap().as_str(), expected, "{input}");
        }
        assert!(configuration_error("postgres://nope").contains("unsupported"));
        assert!(configuration_error("db.turso.io").contains("expected libsql://"));
    }

    #[test]
    fn plaintext_urls_are_allowed_only_for_loopback_hosts() {
        for url in [
            "http://localhost",
            "http://LOCALHOST:8080",
            "http://127.0.0.1:8080",
            "http://127.1.2.3",
            "http://[::1]:8080",
            "http://[::ffff:127.0.0.1]:8080",
            "ws://127.0.0.1:8080",
        ] {
            assert_eq!(pipeline_url(url).unwrap().scheme(), "http", "{url}");
        }
        for url in [
            "http://db.turso.io",
            "http://10.0.0.1:8080",
            "http://[2001:db8::1]:8080",
            "ws://db.turso.io",
            "http://localhost.example.com",
            "http://127.0.0.1.example.com",
        ] {
            let message = configuration_error(url);
            assert!(message.contains("plaintext"), "{url}: {message}");
            assert!(
                message.contains("libsql://, https:// or wss://"),
                "{message}"
            );
        }
    }

    #[test]
    fn urls_without_host_or_with_embedded_secrets_are_rejected() {
        for url in ["https://", "libsql://", "http://:8080"] {
            let message = configuration_error(url);
            assert!(
                message.contains("host") || message.contains("invalid"),
                "{url}: {message}"
            );
        }
        for url in [
            "libsql://db.turso.io?authToken=secret-token",
            "https://user:secret-token@db.turso.io",
            "https://db.turso.io#secret-token",
        ] {
            let message = configuration_error(url);
            assert!(!message.contains("secret-token"), "{message}");
            assert!(
                message.contains("must not contain credentials"),
                "{message}"
            );
        }
    }

    #[test]
    fn store_debug_redacts_token() {
        let store = TursoStateStore::new(configuration("libsql://db.turso.io")).unwrap();
        let rendered = format!("{store:?}");
        assert!(!rendered.contains("secret-token"), "{rendered}");
        assert!(rendered.contains("<redacted>"), "{rendered}");
        assert!(
            rendered.contains("https://db.turso.io/v2/pipeline"),
            "{rendered}"
        );
    }

    #[test]
    fn configuration_debug_redacts_token() {
        let rendered = format!("{:?}", configuration("libsql://db.turso.io"));
        assert!(!rendered.contains("secret-token"));
        assert!(rendered.contains("<redacted>"));
    }

    #[test]
    fn retry_policy_backs_off_exponentially() {
        let policy = RetryPolicy::DEFAULT;
        assert_eq!(policy.retries, 3);
        let delays: Vec<Duration> = (0..policy.retries)
            .map(|retry| policy.delay(retry))
            .collect();
        assert_eq!(delays, [200, 400, 800].map(Duration::from_millis).to_vec());
    }

    #[test]
    fn classifies_http_statuses() {
        for status in [408, 429, 500, 502, 503, 504] {
            let status = reqwest::StatusCode::from_u16(status).unwrap();
            assert!(status_is_transient(status), "{status}");
        }
        for status in [400, 401, 403, 404, 409] {
            let status = reqwest::StatusCode::from_u16(status).unwrap();
            assert!(!status_is_transient(status), "{status}");
        }
    }

    #[test]
    fn classifies_statement_errors() {
        let busy = HranaError {
            message: "database is locked".into(),
            code: Some("SQLITE_BUSY".into()),
        };
        assert!(statement_failure(&busy).is_transient());
        let syntax = HranaError {
            message: "near \"SELEC\": syntax error".into(),
            code: Some("SQLITE_ERROR".into()),
        };
        let failure = statement_failure(&syntax);
        assert_eq!(failure.kind, FailureKind::Statement);
        assert!(failure.message.contains("syntax error (SQLITE_ERROR)"));
    }

    #[test]
    fn describes_error_body_prefixes() {
        let complete = BodyPrefix {
            bytes: b"short".to_vec(),
            truncated: false,
        };
        assert_eq!(describe_error_body(&complete, Some(5)), "short");
        // A two-byte character cut in half at the limit is dropped.
        let mut bytes = "é".repeat(2048).into_bytes();
        bytes.truncate(MAXIMUM_ERROR_BODY_BYTES - 1);
        let cut = BodyPrefix {
            bytes,
            truncated: true,
        };
        let described = describe_error_body(&cut, Some(6000));
        let kept = described.split('…').next().unwrap();
        assert_eq!(kept, "é".repeat(2047));
        assert!(described.ends_with("(truncated, 6000 bytes in total)"));
        assert!(describe_error_body(&cut, None).ends_with("… (truncated)"));
    }

    #[test]
    fn pipeline_body_matches_hrana_wire_format() {
        let statement = Statement::new(
            "SELECT ?",
            vec![
                HranaValue::text("a"),
                HranaValue::integer(7),
                HranaValue::blob(&[1, 2, 3]),
                HranaValue::blob(&[]),
                HranaValue::optional_text(None),
            ],
        );
        let body = PipelineBody {
            baton: None,
            requests: vec![
                PipelineRequest::Execute {
                    statement: &statement,
                },
                PipelineRequest::Close,
            ],
        };
        assert_eq!(
            serde_json::to_value(&body).unwrap(),
            json!({
                "baton": null,
                "requests": [
                    {"type": "execute", "stmt": {"sql": "SELECT ?", "want_rows": true, "args": [
                        {"type": "text", "value": "a"},
                        {"type": "integer", "value": "7"},
                        {"type": "blob", "base64": "AQID"},
                        {"type": "null"},
                        {"type": "null"},
                    ]}},
                    {"type": "close"},
                ],
            })
        );
    }

    #[test]
    fn transaction_batch_matches_hrana_wire_format() {
        let statements = [
            Statement::new("BEGIN IMMEDIATE", Vec::new()),
            Statement::new("ALTER TABLE t ADD COLUMN c", Vec::new()),
            Statement::new("COMMIT", Vec::new()),
            Statement::new("ROLLBACK", Vec::new()),
        ];
        let body = PipelineBody {
            baton: None,
            requests: vec![PipelineRequest::Batch {
                batch: Batch {
                    steps: transaction_steps(&statements, 2),
                },
            }],
        };
        let steps = serde_json::to_value(&body).unwrap()["requests"][0]["batch"]["steps"].clone();
        assert_eq!(steps[0].get("condition"), None);
        assert_eq!(steps[1]["condition"], json!({"type": "ok", "step": 0}));
        assert_eq!(steps[2]["condition"], json!({"type": "ok", "step": 1}));
        assert_eq!(
            steps[3]["condition"],
            json!({"type": "not", "cond": {"type": "ok", "step": 2}})
        );
        assert_eq!(steps[3]["stmt"]["sql"], "ROLLBACK");
    }

    #[test]
    fn interprets_migration_batch_results() {
        let parse = |value: Value| -> BatchResult { serde_json::from_value(value).unwrap() };
        let execute = json!({"cols": [], "rows": [], "affected_row_count": 0});
        let committed = parse(json!({
            "step_results": [execute, execute, execute, null],
            "step_errors": [null, null, null, null],
        }));
        assert_eq!(
            migration_outcome(&committed, 2).unwrap(),
            MigrationOutcome::Applied
        );
        let duplicate = parse(json!({
            "step_results": [execute, null, null, execute],
            "step_errors": [null, {"message": "SQLite error: duplicate column name: tainted", "code": "SQLITE_ERROR"}, null, null],
        }));
        assert_eq!(
            migration_outcome(&duplicate, 2).unwrap(),
            MigrationOutcome::ColumnAlreadyExists
        );
        let broken = parse(json!({
            "step_results": [execute, null, null, execute],
            "step_errors": [null, {"message": "no such table: t", "code": "SQLITE_ERROR"}, null, null],
        }));
        let failure = migration_outcome(&broken, 2).unwrap_err();
        assert_eq!(failure.kind, FailureKind::Statement);
        assert!(failure.message.contains("no such table"));
    }

    #[test]
    fn migrations_are_ordered_and_start_at_one() {
        let versions: Vec<i64> = MIGRATIONS
            .iter()
            .map(|migration| migration.version)
            .collect();
        let expected: Vec<i64> = (1..=i64::try_from(MIGRATIONS.len()).unwrap()).collect();
        assert_eq!(versions, expected);
    }

    #[test]
    fn parses_pipeline_results_and_rows() {
        let response: PipelineResponse = serde_json::from_value(json!({
            "baton": null,
            "base_url": null,
            "results": [
                {"type": "ok", "response": {"type": "execute", "result": {
                    "cols": [{"name": "a", "decltype": "TEXT"}],
                    "rows": [[
                        {"type": "text", "value": "random_pet"},
                        {"type": "integer", "value": "3"},
                        {"type": "blob", "base64": "AQID"},
                        {"type": "null"},
                    ]],
                    "affected_row_count": 0,
                    "last_insert_rowid": null,
                    "rows_read": 1,
                }}},
                {"type": "error", "error": {"message": "boom", "code": "SQLITE_ERROR"}},
                {"type": "ok", "response": {"type": "close"}},
            ],
        }))
        .unwrap();
        let PipelineResult::Ok {
            response: StreamResponse::Execute { result },
        } = &response.results[0]
        else {
            panic!("expected execute result");
        };
        let row = &result.rows[0];
        assert_eq!(row[0].as_text(), Some("random_pet"));
        assert_eq!(row[1].as_integer(), Some(3));
        assert_eq!(row[2].as_blob().unwrap(), Some(vec![1, 2, 3]));
        assert_eq!(row[3].as_blob().unwrap(), None);
        assert!(matches!(response.results[1], PipelineResult::Error { .. }));
        assert!(matches!(
            response.results[2],
            PipelineResult::Ok {
                response: StreamResponse::Other
            }
        ));
    }

    #[test]
    fn row_to_resource_reads_tainted_and_identity() {
        let row = |tainted: &str, identity: HranaValue| {
            vec![
                HranaValue::text("random_pet"),
                HranaValue::text("pet"),
                HranaValue::text("random"),
                HranaValue::text("registry.terraform.io/hashicorp/random"),
                HranaValue::integer(1),
                HranaValue::text("{\"id\":\"x\"}"),
                HranaValue::Null,
                HranaValue::text("[]"),
                HranaValue::Integer {
                    value: tainted.into(),
                },
                identity,
            ]
        };
        let resource = row_to_resource(&row("1", HranaValue::text("{\"name\":\"a\"}"))).unwrap();
        assert!(resource.tainted);
        assert_eq!(resource.identity, Some(json!({"name": "a"})));
        let resource = row_to_resource(&row("0", HranaValue::Null)).unwrap();
        assert!(!resource.tainted);
        assert_eq!(resource.identity, None);
    }

    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "requires a libSQL server (CUENV_INFRASTRUCTURE_TEST_TURSO_URL)"]
    async fn concurrent_migrations_converge() {
        let url = std::env::var("CUENV_INFRASTRUCTURE_TEST_TURSO_URL").unwrap();
        let store = Arc::new(
            TursoStateStore::new(TursoConfiguration {
                url,
                authentication_token: std::env::var("TURSO_AUTH_TOKEN").ok(),
            })
            .unwrap(),
        );
        let mut migrators = tokio::task::JoinSet::new();
        for _ in 0..8 {
            let store = Arc::clone(&store);
            migrators.spawn(async move { store.migrate().await });
        }
        while let Some(outcome) = migrators.join_next().await {
            outcome.unwrap().unwrap();
        }
        let latest = MIGRATIONS.last().unwrap().version;
        assert_eq!(store.schema_version().await.unwrap(), latest);
        store.migrate().await.unwrap();
        assert_eq!(store.schema_version().await.unwrap(), latest);
    }

    /// Against a real server: reads on a database cuenv never touched are
    /// empty and create nothing, and a schema newer than this build is
    /// refused everywhere. The database must be one nothing else uses; the
    /// test drops cuenv's tables at the end so it can run again.
    #[tokio::test(flavor = "multi_thread")]
    #[ignore = "requires an otherwise unused libSQL database (CUENV_INFRASTRUCTURE_TEST_FRESH_TURSO_URL)"]
    async fn fresh_database_reads_and_newer_schema_against_a_server() {
        let url = std::env::var("CUENV_INFRASTRUCTURE_TEST_FRESH_TURSO_URL").unwrap();
        let store = TursoStateStore::new(TursoConfiguration {
            url,
            authentication_token: std::env::var("TURSO_AUTH_TOKEN").ok(),
        })
        .unwrap();
        let drop_tables = || async {
            for table in [SCHEMA_TABLE, RESOURCES_TABLE, LOCKS_TABLE] {
                store
                    .execute(Statement::new(
                        format!("DROP TABLE IF EXISTS {table}"),
                        Vec::new(),
                    ))
                    .await
                    .unwrap();
            }
        };
        let untouched = StoredSchema {
            version: 0,
            resources_table: false,
            locks_table: false,
        };
        assert_eq!(
            store.stored_schema().await.unwrap(),
            untouched,
            "the database must start without cuenv tables"
        );

        assert_eq!(store.list(&tenant()).await.unwrap(), Vec::new());
        assert_eq!(store.current_lock(&tenant()).await.unwrap(), None);
        assert!(!store.force_unlock(&tenant(), "any").await.unwrap());
        assert!(store.lock(&tenant(), "test").await.is_err());
        assert_eq!(store.stored_schema().await.unwrap(), untouched);

        store.migrate().await.unwrap();
        let lock = store.lock(&tenant(), "test").await.unwrap();
        store.unlock(&tenant(), &lock).await.unwrap();

        store
            .execute(Statement::new(
                "INSERT INTO cuenv_infrastructure_schema (version) VALUES (?)",
                vec![HranaValue::integer(LATEST_SCHEMA_VERSION + 1)],
            ))
            .await
            .unwrap();
        let refusals = [
            store.migrate().await.err(),
            store.list(&tenant()).await.err(),
            store.current_lock(&tenant()).await.err(),
            store.lock(&tenant(), "test").await.err(),
            store.force_unlock(&tenant(), "any").await.err(),
        ];
        drop_tables().await;
        for refusal in refusals {
            let message = refusal.expect("a newer schema must be refused").to_string();
            assert!(
                message.contains("newer than this cuenv supports"),
                "{message}"
            );
        }
    }

    // -----------------------------------------------------------------------
    // A minimal HTTP server standing in for Turso.
    // -----------------------------------------------------------------------

    /// How the fake server answers one request: `None` drops the connection.
    type Responder = dyn Fn(usize, Value) -> Option<(u16, String)> + Send + Sync;

    struct FakeServer {
        url: String,
        requests: Arc<AtomicUsize>,
    }

    async fn fake_server(responder: Arc<Responder>) -> FakeServer {
        fake_server_with_headers(responder, String::new()).await
    }

    /// A fake server whose every reply also carries `headers` (each line
    /// ending in `\r\n`).
    async fn fake_server_with_headers(responder: Arc<Responder>, headers: String) -> FakeServer {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&requests);
        let replies = Arc::new(Replies { responder, headers });
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let index = counter.fetch_add(1, Ordering::SeqCst);
                tokio::spawn(answer(stream, index, Arc::clone(&replies)));
            }
        });
        FakeServer { url, requests }
    }

    /// How a fake server replies.
    struct Replies {
        responder: Arc<Responder>,
        headers: String,
    }

    async fn answer(mut stream: TcpStream, index: usize, replies: Arc<Replies>) {
        let Replies { responder, headers } = replies.as_ref();
        let mut buffer = Vec::new();
        let mut chunk = [0_u8; 4096];
        let header_end = loop {
            let read = stream.read(&mut chunk).await.unwrap();
            if read == 0 {
                return;
            }
            buffer.extend_from_slice(&chunk[..read]);
            if let Some(position) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
                break position + 4;
            }
        };
        let request_headers = String::from_utf8_lossy(&buffer[..header_end]).to_ascii_lowercase();
        let length: usize = request_headers
            .lines()
            .find_map(|line| line.strip_prefix("content-length:"))
            .map_or(0, |value| value.trim().parse().unwrap());
        while buffer.len() < header_end + length {
            let read = stream.read(&mut chunk).await.unwrap();
            if read == 0 {
                return;
            }
            buffer.extend_from_slice(&chunk[..read]);
        }
        let body: Value = serde_json::from_slice(&buffer[header_end..header_end + length]).unwrap();
        let Some((status, response)) = responder(index, body) else {
            return;
        };
        let reply = format!(
            "HTTP/1.1 {status} Status\r\ncontent-type: application/json\r\n{headers}\
             content-length: {}\r\nconnection: close\r\n\r\n{response}",
            response.len()
        );
        stream.write_all(reply.as_bytes()).await.unwrap();
        stream.shutdown().await.unwrap();
    }

    fn execute_response(rows: &Value, affected_row_count: u64) -> String {
        json!({
            "baton": null,
            "base_url": null,
            "results": [
                {"type": "ok", "response": {"type": "execute", "result": {
                    "cols": [], "rows": rows, "affected_row_count": affected_row_count,
                }}},
                {"type": "ok", "response": {"type": "close"}},
            ],
        })
        .to_string()
    }

    /// Answer every statement of a pipeline with `rows_for(sql)`; `None` is
    /// a statement error.
    fn pipeline_response(body: &Value, rows_for: impl Fn(&str) -> Option<Value>) -> String {
        let results: Vec<Value> = body["requests"]
            .as_array()
            .unwrap()
            .iter()
            .map(|request| match request["type"].as_str().unwrap() {
                "execute" => {
                    let sql = request["stmt"]["sql"].as_str().unwrap();
                    rows_for(sql).map_or_else(
                        || {
                            json!({"type": "error", "error": {
                                "message": format!("unexpected statement: {sql}"),
                                "code": "SQLITE_ERROR",
                            }})
                        },
                        |rows| {
                            json!({"type": "ok", "response": {"type": "execute", "result": {
                                "cols": [], "rows": rows, "affected_row_count": 1,
                            }}})
                        },
                    )
                }
                _ => json!({"type": "ok", "response": {"type": "close"}}),
            })
            .collect();
        json!({"baton": null, "base_url": null, "results": results}).to_string()
    }

    /// Rows a database at schema `version` with every cuenv table returns
    /// for the store's schema inspection; `None` for any other statement.
    fn schema_rows(sql: &str, version: i64) -> Option<Value> {
        if sql.starts_with("SELECT name FROM sqlite_master") {
            Some(json!([
                [{"type": "text", "value": SCHEMA_TABLE}],
                [{"type": "text", "value": RESOURCES_TABLE}],
                [{"type": "text", "value": LOCKS_TABLE}],
            ]))
        } else if sql == SELECT_SCHEMA_VERSION {
            Some(json!([[{"type": "integer", "value": version.to_string()}]]))
        } else {
            None
        }
    }

    fn first_statement(body: &Value) -> String {
        body["requests"][0]["stmt"]["sql"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn fast_store(url: &str, timeout: Duration) -> TursoStateStore {
        let mut store = TursoStateStore::with_timeout(configuration(url), timeout).unwrap();
        store.retry_policy = RetryPolicy {
            retries: 3,
            initial_delay: Duration::from_millis(1),
        };
        store
    }

    fn tenant() -> TenantKey {
        TenantKey::new("example.com/fake", "web").unwrap()
    }

    #[tokio::test]
    async fn retries_transient_http_statuses_then_succeeds() {
        let server = fake_server(Arc::new(|index, _| {
            Some(match index {
                0 => (503, "unavailable".to_string()),
                1 => (429, "slow down".to_string()),
                _ => (200, execute_response(&json!([]), 0)),
            })
        }))
        .await;
        let store = fast_store(&server.url, Duration::from_secs(5));
        assert_eq!(store.list(&tenant()).await.unwrap(), Vec::new());
        assert_eq!(server.requests.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn permanent_http_errors_are_not_retried_and_never_leak_the_token() {
        let server = fake_server(Arc::new(|_, _| {
            let echo = format!("bad token secret-token {}", "x".repeat(10_000));
            Some((401, echo))
        }))
        .await;
        let store = fast_store(&server.url, Duration::from_secs(5));
        let message = store.list(&tenant()).await.unwrap_err().to_string();
        assert_eq!(server.requests.load(Ordering::SeqCst), 1);
        assert!(message.contains("HTTP 401"), "{message}");
        assert!(!message.contains("secret-token"), "{message}");
        assert!(message.contains("<redacted>"), "{message}");
        assert!(message.contains("truncated"), "{message}");
        assert!(
            message.len() < MAXIMUM_ERROR_BODY_BYTES + 512,
            "{}",
            message.len()
        );
    }

    #[tokio::test]
    async fn connection_failures_are_retried_and_report_their_cause() {
        // Bind then drop a listener to find a port nothing listens on.
        let port = TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let store = fast_store(&format!("http://127.0.0.1:{port}"), Duration::from_secs(5));
        let message = store.list(&tenant()).await.unwrap_err().to_string();
        assert!(message.contains("gave up after 4 attempts"), "{message}");
        // The source chain is included, not just "error sending request".
        assert!(message.to_lowercase().contains("connect"), "{message}");
        assert!(message.matches(": ").count() >= 2, "{message}");
    }

    #[tokio::test]
    async fn timeouts_are_retried_and_reported_as_timeouts() {
        // Accept connections and never answer them.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let accepted = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&accepted);
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                counter.fetch_add(1, Ordering::SeqCst);
                // Read until the client gives up, never answering.
                tokio::spawn(async move {
                    let mut ignored = Vec::new();
                    let _ = stream.read_to_end(&mut ignored).await;
                });
            }
        });
        let store = fast_store(&url, Duration::from_millis(100));
        let message = store.list(&tenant()).await.unwrap_err().to_string();
        assert!(message.contains("timed out"), "{message}");
        assert!(message.contains("gave up after 4 attempts"), "{message}");
        assert_eq!(accepted.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn lock_insert_with_lost_response_is_recovered_from_the_lock_row() {
        let inserted = Arc::new(Mutex::new(None::<String>));
        let recorded = Arc::clone(&inserted);
        let inserts = Arc::new(AtomicUsize::new(0));
        let insert_counter = Arc::clone(&inserts);
        let server = fake_server(Arc::new(move |_, body| {
            let sql = first_statement(&body);
            if schema_rows(&sql, LATEST_SCHEMA_VERSION).is_some() {
                return Some((
                    200,
                    pipeline_response(&body, |sql| schema_rows(sql, LATEST_SCHEMA_VERSION)),
                ));
            }
            if sql.starts_with("INSERT INTO cuenv_infrastructure_locks") {
                insert_counter.fetch_add(1, Ordering::SeqCst);
                // Commit the lock, then lose the response.
                let identifier = body["requests"][0]["stmt"]["args"][2]["value"]
                    .as_str()
                    .unwrap()
                    .to_string();
                *recorded.lock().unwrap() = Some(identifier);
                return None;
            }
            let rows = recorded
                .lock()
                .unwrap()
                .as_ref()
                .map_or(json!([]), |identifier| {
                    json!([[
                        {"type": "text", "value": identifier},
                        {"type": "text", "value": "test"},
                        {"type": "text", "value": "2026-01-01T00:00:00Z"},
                    ]])
                });
            Some((200, execute_response(&rows, 0)))
        }))
        .await;
        let store = fast_store(&server.url, Duration::from_secs(5));
        let lock = store.lock(&tenant(), "test").await.unwrap();
        assert_eq!(Some(lock.lock_identifier), inserted.lock().unwrap().clone());
        // One insert and one read after the schema check: no blind retry of
        // the insert.
        assert_eq!(inserts.load(Ordering::SeqCst), 1);
        assert_eq!(server.requests.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn lock_held_by_another_run_is_reported() {
        let server = fake_server(Arc::new(|_, body| {
            let sql = first_statement(&body);
            Some(if schema_rows(&sql, LATEST_SCHEMA_VERSION).is_some() {
                (
                    200,
                    pipeline_response(&body, |sql| schema_rows(sql, LATEST_SCHEMA_VERSION)),
                )
            } else if sql.starts_with("INSERT") {
                (200, execute_response(&json!([]), 0))
            } else {
                let rows = json!([[
                    {"type": "text", "value": "other-lock"},
                    {"type": "text", "value": "someone else"},
                    {"type": "text", "value": "2026-01-01T00:00:00Z"},
                ]]);
                (200, execute_response(&rows, 0))
            })
        }))
        .await;
        let store = fast_store(&server.url, Duration::from_secs(5));
        let error = store.lock(&tenant(), "test").await.unwrap_err();
        assert!(
            matches!(&error, InfrastructureError::Locked { lock_identifier, holder, .. }
                if lock_identifier == "other-lock" && holder == "someone else"),
            "{error:?}"
        );
    }

    /// A fake database that answers with `rows_for` and records every
    /// statement it receives.
    async fn recording_database(
        rows_for: impl Fn(&str) -> Option<Value> + Send + Sync + 'static,
    ) -> (FakeServer, Arc<Mutex<Vec<String>>>) {
        let statements = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&statements);
        let server = fake_server(Arc::new(move |_, body| {
            let requests = body["requests"].as_array().unwrap();
            recorded.lock().unwrap().extend(
                requests
                    .iter()
                    .filter_map(|request| request["stmt"]["sql"].as_str())
                    .map(str::to_string),
            );
            Some((200, pipeline_response(&body, &rows_for)))
        }))
        .await;
        (server, statements)
    }

    #[tokio::test]
    async fn reads_on_a_database_without_cuenv_tables_are_empty_and_change_nothing() {
        let (server, statements) = recording_database(|sql| {
            sql.starts_with("SELECT name FROM sqlite_master")
                .then(|| json!([]))
        })
        .await;
        let store = fast_store(&server.url, Duration::from_secs(5));
        assert_eq!(store.list(&tenant()).await.unwrap(), Vec::new());
        assert_eq!(store.current_lock(&tenant()).await.unwrap(), None);
        assert!(!store.force_unlock(&tenant(), "any").await.unwrap());
        let statements = statements.lock().unwrap().clone();
        assert_eq!(statements.len(), 3, "{statements:?}");
        assert!(
            statements
                .iter()
                .all(|sql| sql.starts_with("SELECT name FROM sqlite_master")),
            "{statements:?}"
        );
        // Taking the lock needs a migrated schema.
        let message = store.lock(&tenant(), "test").await.unwrap_err().to_string();
        assert!(message.contains("migrate the state store"), "{message}");
    }

    #[tokio::test]
    async fn every_operation_refuses_a_newer_schema() {
        let newer = LATEST_SCHEMA_VERSION + 1;
        let (server, statements) = recording_database(move |sql| {
            schema_rows(sql, newer).or_else(|| (sql == CREATE_SCHEMA_TABLE).then(|| json!([])))
        })
        .await;
        let store = fast_store(&server.url, Duration::from_secs(5));
        let messages = [
            store.migrate().await.unwrap_err().to_string(),
            store.list(&tenant()).await.unwrap_err().to_string(),
            store.current_lock(&tenant()).await.unwrap_err().to_string(),
            store.lock(&tenant(), "test").await.unwrap_err().to_string(),
            store
                .force_unlock(&tenant(), "lock")
                .await
                .unwrap_err()
                .to_string(),
        ];
        for message in messages {
            assert!(
                message.contains(&format!(
                    "schema version {newer} is newer than this cuenv supports"
                )),
                "{message}"
            );
        }
        // Nothing but schema inspection reached the database.
        let statements = statements.lock().unwrap().clone();
        assert!(
            statements.iter().all(|sql| sql == CREATE_SCHEMA_TABLE
                || sql == SELECT_SCHEMA_VERSION
                || sql.starts_with("SELECT name FROM sqlite_master")),
            "{statements:?}"
        );
    }

    #[tokio::test]
    async fn reads_an_older_schema_without_migrating_it() {
        let row = json!([[
            {"type": "text", "value": "random_pet"},
            {"type": "text", "value": "pet"},
            {"type": "text", "value": "random"},
            {"type": "text", "value": "registry.terraform.io/hashicorp/random"},
            {"type": "integer", "value": "0"},
            {"type": "text", "value": "{\"id\":\"x\"}"},
            {"type": "null"},
            {"type": "text", "value": "[]"},
            {"type": "integer", "value": "0"},
            {"type": "null"},
        ]]);
        let (server, statements) = recording_database(move |sql| {
            schema_rows(sql, 1).or_else(|| {
                (sql == SELECT_RESOURCES_WITHOUT_TAINT_AND_IDENTITY).then(|| row.clone())
            })
        })
        .await;
        let store = fast_store(&server.url, Duration::from_secs(5));
        let resources = store.list(&tenant()).await.unwrap();
        assert_eq!(resources.len(), 1);
        assert!(!resources[0].tainted);
        assert_eq!(resources[0].identity, None);
        let message = store.lock(&tenant(), "test").await.unwrap_err().to_string();
        assert!(message.contains("version 1"), "{message}");
        assert!(
            statements
                .lock()
                .unwrap()
                .iter()
                .all(|sql| !sql.starts_with("ALTER") && !sql.starts_with("CREATE")),
        );
    }

    #[tokio::test]
    async fn redirects_are_not_followed() {
        let elsewhere = fake_server(Arc::new(|_, _| {
            Some((200, execute_response(&json!([]), 0)))
        }))
        .await;
        let redirecting = fake_server_with_headers(
            Arc::new(|_, _| Some((307, String::new()))),
            format!("location: {}/v2/pipeline\r\n", elsewhere.url),
        )
        .await;
        let store = fast_store(&redirecting.url, Duration::from_secs(5));
        let message = store.list(&tenant()).await.unwrap_err().to_string();
        assert!(message.contains("HTTP 307"), "{message}");
        assert_eq!(redirecting.requests.load(Ordering::SeqCst), 1);
        assert_eq!(elsewhere.requests.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn oversized_responses_are_refused_without_retrying() {
        let server = fake_server(Arc::new(|_, body| {
            Some((
                200,
                pipeline_response(&body, |_| {
                    Some(json!([[{"type": "text", "value": "x".repeat(1_000)}]]))
                }),
            ))
        }))
        .await;
        let mut store = fast_store(&server.url, Duration::from_secs(5));
        store.maximum_response_bytes = 512;
        let message = store.list(&tenant()).await.unwrap_err().to_string();
        assert!(message.contains("exceeds the 512-byte limit"), "{message}");
        assert_eq!(server.requests.load(Ordering::SeqCst), 1);
    }

    /// Set in the child process of
    /// [`plaintext_loopback_requests_bypass_proxy_environment`].
    const PROXY_CHILD_VARIABLE: &str = "CUENV_INFRASTRUCTURE_TEST_PROXY_CHILD";

    /// A plaintext URL is always loopback; its requests (and the token) must
    /// never go to a proxy named in the environment. The proxy variables are
    /// process-wide, so the check runs in a child process of this test binary.
    #[tokio::test]
    async fn plaintext_loopback_requests_bypass_proxy_environment() {
        if std::env::var_os(PROXY_CHILD_VARIABLE).is_some() {
            let server = fake_server(Arc::new(|_, body| {
                Some((200, pipeline_response(&body, |_| Some(json!([])))))
            }))
            .await;
            let store = fast_store(&server.url, Duration::from_secs(5));
            assert_eq!(store.list(&tenant()).await.unwrap(), Vec::new());
            assert_eq!(server.requests.load(Ordering::SeqCst), 1);
            return;
        }
        let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_url = format!("http://{}", proxy.local_addr().unwrap());
        let proxied = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&proxied);
        tokio::spawn(async move {
            // Accept and drop: a request sent here fails.
            while let Ok((stream, _)) = proxy.accept().await {
                counter.fetch_add(1, Ordering::SeqCst);
                drop(stream);
            }
        });
        let output = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "state::turso::tests::plaintext_loopback_requests_bypass_proxy_environment",
                "--test-threads=1",
            ])
            .env(PROXY_CHILD_VARIABLE, "1")
            .env("HTTP_PROXY", &proxy_url)
            .env("http_proxy", &proxy_url)
            .env("ALL_PROXY", &proxy_url)
            .env("all_proxy", &proxy_url)
            .env_remove("NO_PROXY")
            .env_remove("no_proxy")
            .output()
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("1 passed"),
            "the child did not run the test"
        );
        assert_eq!(proxied.load(Ordering::SeqCst), 0);
    }
}
