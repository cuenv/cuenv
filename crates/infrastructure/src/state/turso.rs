//! Turso (libSQL) state store over the Hrana HTTP protocol.
//!
//! Talks to `POST {url}/v2/pipeline` with bearer-token authentication, so it works
//! against Turso Cloud databases and self-hosted `sqld` alike without a
//! native libSQL dependency.
//!
//! One table family holds every identity: resources, locks and owners are keyed
//! by `(module_path, project, environment, ...)`, with the environment empty
//! for a run without `--env`. The no-flag identity and each named environment
//! are separate tenants; nothing falls back from one to another.
//!
//! The schema is versioned: `cuenv_infrastructure_migrations` records the
//! newest migration applied, and [`StateStore::migrate`] applies only newer
//! ones, each in its own transaction. A migration after the first refuses,
//! inside its transaction, while any lock row exists, so a run that is
//! applying changes never has its writes land in a half-migrated shape; it
//! waits a bounded time for the locks to drain (announcing itself so that no
//! new lock is taken meanwhile) and then reports every lock that blocks it.
//! Every operation refuses a database whose schema is newer than this build
//! knows, and a database holding the tables of an unreleased development
//! build (which recorded versions 1 to 5 in `cuenv_infrastructure_schema`).
//! Reads
//! ([`StateStore::list`] and [`StateStore::current_lock`]) never migrate: on a
//! database without cuenv's tables they return nothing, so a read-only token can
//! plan and inspect state. Taking the lock requires the current schema.
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
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use serde::{Deserialize, Serialize};

use super::{
    ConditionalPut, LockInformation, LockRequest, ManagedResource, OwnerClaim, OwnerClaimMode,
    RecordVersion, ResourceAddress, StateLock, StateStore, TenantLock, TenantOwner,
};
use crate::error::{
    InfrastructureError, Result, describe_json_error, json_error_category, strip_control_characters,
};
use crate::tenant::{ProjectInstance, TenantKey};

/// One schema migration: the statements that move the database to `version`.
struct Migration {
    version: i64,
    statements: &'static [&'static str],
}

/// Ordered schema migrations. Never edit an existing entry; append a new one.
///
/// Version 1 is the first schema anyone is meant to use. No released cuenv
/// ever wrote an earlier layout; a database holding the tables of an
/// unreleased development build is refused by name (see
/// [`UNRELEASED_LAYOUT_TABLES`]) instead of being adopted or silently
/// ignored.
const MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    statements: &[
        "CREATE TABLE cuenv_infrastructure_resources (
            module_path TEXT NOT NULL,
            project TEXT NOT NULL,
            environment TEXT NOT NULL,
            resource_type TEXT NOT NULL,
            resource_name TEXT NOT NULL,
            provider TEXT NOT NULL,
            provider_source TEXT NOT NULL,
            schema_version INTEGER NOT NULL,
            state_json TEXT NOT NULL,
            private BLOB,
            dependencies_json TEXT NOT NULL DEFAULT '[]',
            tainted INTEGER NOT NULL DEFAULT 0,
            identity_json TEXT,
            serial INTEGER NOT NULL DEFAULT 1,
            generation TEXT NOT NULL,
            created_at TEXT NOT NULL,
            updated_at TEXT NOT NULL,
            PRIMARY KEY (module_path, project, environment, resource_type, resource_name)
        ) WITHOUT ROWID",
        "CREATE TABLE cuenv_infrastructure_locks (
            module_path TEXT NOT NULL,
            project TEXT NOT NULL,
            environment TEXT NOT NULL,
            lock_identifier TEXT NOT NULL,
            holder TEXT NOT NULL,
            acquired_at TEXT NOT NULL,
            PRIMARY KEY (module_path, project, environment)
        ) WITHOUT ROWID",
        "CREATE TABLE cuenv_infrastructure_owners (
            module_path TEXT NOT NULL,
            project TEXT NOT NULL,
            environment TEXT NOT NULL,
            instance TEXT NOT NULL,
            claimed_at TEXT NOT NULL,
            PRIMARY KEY (module_path, project, environment)
        ) WITHOUT ROWID",
        "CREATE TABLE cuenv_infrastructure_pending_migration (
            version INTEGER NOT NULL PRIMARY KEY,
            expires_at INTEGER NOT NULL
        )",
    ],
}];

/// The first schema version. It creates the lock table, so only later
/// migrations can be fenced by it.
const INITIAL_SCHEMA_VERSION: i64 = 1;

/// Newest schema version this build knows.
const LATEST_SCHEMA_VERSION: i64 = MIGRATIONS[MIGRATIONS.len() - 1].version;

/// Records the migrations applied. Unreleased development builds recorded
/// their (different) versions 1 to 5 in `cuenv_infrastructure_schema`; this
/// table has another name so those never collide with the versions this one
/// records, and a database that has only the old table is recognised as theirs.
const SCHEMA_TABLE: &str = "cuenv_infrastructure_migrations";

/// The data tables version 1 creates.
const DATA_TABLES: [&str; 4] = [
    "cuenv_infrastructure_resources",
    "cuenv_infrastructure_locks",
    "cuenv_infrastructure_owners",
    "cuenv_infrastructure_pending_migration",
];

/// Names that only unreleased development builds of cuenv created: their
/// schema table, their per-environment tables, and the misspelled table
/// family of the earliest layout (matched by prefix).
const UNRELEASED_LAYOUT_TABLES: [&str; 1] = ["cuenv_infrastructure_schema"];

/// Name prefixes of the tables unreleased development builds created.
const UNRELEASED_LAYOUT_PREFIXES: [&str; 2] = [
    "cuenv_infrastructure_environment_",
    "cuenv_infrastructurestructure_",
];

/// The `environment` column of a run without `--env`. A named environment is
/// never empty ([`TenantKey::with_environment`]), so the two cannot collide.
const NO_ENVIRONMENT: &str = "";

const CREATE_SCHEMA_TABLE: &str =
    "CREATE TABLE IF NOT EXISTS cuenv_infrastructure_migrations (version INTEGER NOT NULL)";

const SELECT_SCHEMA_VERSION: &str =
    "SELECT COALESCE(MAX(version), 0) FROM cuenv_infrastructure_migrations";

/// Every table whose name starts like one of cuenv's (`_` also matches any
/// single character, which only widens the search).
const SELECT_CUENV_TABLES: &str = "SELECT name FROM sqlite_master WHERE type = 'table' \
     AND name LIKE 'cuenv_infrastructure%' ORDER BY name";

/// Every lock of every tenant, oldest first.
const SELECT_ALL_LOCKS: &str = "SELECT module_path, project, environment, lock_identifier, \
     holder, acquired_at FROM cuenv_infrastructure_locks \
     ORDER BY acquired_at, module_path, project, environment";

/// The key columns of one tenant's records.
const SELECT_ADDRESSES: &str = "SELECT resource_type, resource_name \
     FROM cuenv_infrastructure_resources \
     WHERE module_path = ? AND project = ? AND environment = ? \
     ORDER BY resource_type, resource_name";

/// The migration a waiting migrator announced, when its announcement has not
/// expired. One argument: the current time in seconds since the epoch.
const SELECT_PENDING_MIGRATION: &str = "SELECT version FROM cuenv_infrastructure_pending_migration \
     WHERE expires_at > ?";

const SELECT_RESOURCES: &str = "SELECT resource_type, resource_name, provider, provider_source, \
     schema_version, state_json, private, dependencies_json, tainted, identity_json, serial, \
     generation FROM cuenv_infrastructure_resources \
     WHERE module_path = ? AND project = ? AND environment = ? \
     ORDER BY resource_type, resource_name";

/// One record of a tenant, with the columns of [`SELECT_RESOURCES`].
const SELECT_RESOURCE: &str = "SELECT resource_type, resource_name, provider, provider_source, \
     schema_version, state_json, private, dependencies_json, tainted, identity_json, serial, \
     generation FROM cuenv_infrastructure_resources \
     WHERE module_path = ? AND project = ? AND environment = ? \
     AND resource_type = ? AND resource_name = ?";

const SELECT_OWNER: &str = "SELECT instance, claimed_at FROM cuenv_infrastructure_owners \
     WHERE module_path = ? AND project = ? AND environment = ?";

const SELECT_LOCK: &str = "SELECT lock_identifier, holder, acquired_at \
     FROM cuenv_infrastructure_locks \
     WHERE module_path = ? AND project = ? AND environment = ?";

/// Arguments: the tenant, then the lock identifier.
const LOCK_HELD: &str = "EXISTS (SELECT 1 FROM cuenv_infrastructure_locks \
     WHERE module_path = ? AND project = ? AND environment = ? AND lock_identifier = ?)";

/// Arguments: the tenant, the address, the columns of
/// [`RecordColumns::insert_arguments`]. Callers append a lock check and a
/// conflict clause.
const INSERT_RESOURCE: &str = "INSERT INTO cuenv_infrastructure_resources \
     (module_path, project, environment, resource_type, resource_name, provider, \
     provider_source, schema_version, state_json, private, dependencies_json, tainted, \
     identity_json, serial, created_at, updated_at, generation) \
     SELECT ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 1, ?, ?, ?";

const RESOURCE_KEY: &str = "module_path, project, environment, resource_type, resource_name";

const TENANT_KEY: &str = "module_path, project, environment";

/// Largest HTTP error body read (and then only parsed for an error code,
/// never quoted), in bytes.
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

/// How a migration that finds locks held waits for them, while keeping new
/// locks from being taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MigrationWait {
    /// The longest a migration waits for the locks to be released.
    bound: Duration,
    /// How often it looks again.
    poll_interval: Duration,
    /// How long its announcement (which holds off new locks) stands if the
    /// migrating process dies; always longer than `bound`.
    announcement_lifetime: Duration,
}

impl MigrationWait {
    const DEFAULT: Self = Self {
        bound: Duration::from_secs(30),
        poll_interval: Duration::from_millis(500),
        announcement_lifetime: Duration::from_secs(60),
    };
}

/// [`StateStore`] backed by a remote Turso database.
#[derive(Clone)]
pub struct TursoStateStore {
    client: reqwest::Client,
    pipeline_url: reqwest::Url,
    authentication_token: Option<String>,
    retry_policy: RetryPolicy,
    /// How a migration waits for the locks that block it.
    migration_wait: MigrationWait,
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
            .field("migration_wait", &self.migration_wait)
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
            migration_wait: MigrationWait::DEFAULT,
            maximum_response_bytes: MAXIMUM_RESPONSE_BYTES,
        })
    }

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
                message: describe_http_error(&HttpError {
                    status,
                    body: &body,
                    total_bytes,
                    disclosure,
                }),
            });
        }
        let bytes = read_whole_body(&mut response, self.maximum_response_bytes).await?;
        serde_json::from_slice(&bytes).map_err(|error| Failure {
            kind: FailureKind::Permanent,
            message: format!("invalid Turso response ({})", describe_json_error(&error)),
        })
    }

    /// Execute statements in one pipeline request, without retrying.
    ///
    /// Statements run in order, each in its own implicit transaction.
    async fn pipeline_once(&self, statements: &[Statement]) -> Attempted<Vec<ExecuteResult>> {
        let count = statements.len();
        let disclosure = Disclosure::of(statements);
        let requests = statements
            .iter()
            .map(|statement| PipelineRequest::Execute { statement })
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
                } => results.push(result),
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

    /// Execute a conditional batch in one pipeline request, without retrying.
    async fn batch_once(&self, steps: Vec<BatchStep<'_>>) -> Attempted<BatchResult> {
        let disclosure = if steps
            .iter()
            .all(|step| step.statement.disclosure == Disclosure::Full)
        {
            Disclosure::Full
        } else {
            Disclosure::CodeOnly
        };
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
    /// Creates the schema table, so only the migration path calls it. Refuses
    /// first a database that holds tables of another layout.
    async fn schema_version(&self) -> Result<i64> {
        Self::require_recorded_or_recognized(&self.cuenv_tables().await?)?;
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

    /// The tables whose names start like cuenv's, with one request.
    async fn cuenv_tables(&self) -> Result<Vec<String>> {
        Ok(self
            .execute(Statement::new(SELECT_CUENV_TABLES, Vec::new()))
            .await?
            .rows
            .iter()
            .filter_map(|row| row.first().and_then(HranaValue::as_text))
            .map(strip_control_characters)
            .collect())
    }

    /// [`Self::require_recognized_layout`] unless the schema table is there.
    fn require_recorded_or_recognized(names: &[String]) -> Result<()> {
        if names.iter().any(|name| name == SCHEMA_TABLE) {
            Ok(())
        } else {
            Self::require_recognized_layout(names)
        }
    }

    /// Refuse a database that has no schema table of this cuenv but holds
    /// tables of another layout: those an unreleased development build wrote
    /// ([`InfrastructureError::StateUnreleasedLayout`]), or this cuenv's own
    /// table names without a record of the migration that created them
    /// ([`InfrastructureError::StateSchemaConflict`]). Neither is adopted and
    /// neither is ignored.
    fn require_recognized_layout(names: &[String]) -> Result<()> {
        match classify_layout(names) {
            Layout::Recognized => Ok(()),
            Layout::Unreleased(tables) => {
                Err(InfrastructureError::StateUnreleasedLayout { tables })
            }
            Layout::Unrecorded(tables) => Err(InfrastructureError::StateSchemaConflict {
                problem: format!(
                    "it holds tables with cuenv's names ({}) but no record of the migration \
                     that created them ({SCHEMA_TABLE}), so this cuenv did not create them; \
                     use another database or remove those tables",
                    tables.join(", ")
                ),
            }),
        }
    }

    /// The recorded schema version, read without creating or changing
    /// anything; 0 when cuenv never migrated the database.
    ///
    /// Fails closed on a schema newer than this build knows, so an older
    /// cuenv never reads or writes rows whose meaning may have changed, and
    /// on a database that holds tables of another layout.
    async fn stored_version(&self) -> Result<i64> {
        let names = self.cuenv_tables().await?;
        if !names.iter().any(|name| name == SCHEMA_TABLE) {
            Self::require_recognized_layout(&names)?;
            return Ok(0);
        }
        let version = self
            .execute(Statement::new(SELECT_SCHEMA_VERSION, Vec::new()))
            .await?
            .rows
            .first()
            .and_then(|row| row.first())
            .and_then(HranaValue::as_integer)
            .ok_or_else(|| InfrastructureError::state("Turso returned no schema version"))?;
        if version > LATEST_SCHEMA_VERSION {
            return Err(newer_schema(version, LATEST_SCHEMA_VERSION));
        }
        Ok(version)
    }

    /// Refuse unless the recorded schema is exactly the one this build
    /// writes: [`InfrastructureError::StateSchemaNewer`] for a newer one,
    /// a state error asking for a migration for an older or missing one.
    async fn require_current_schema(&self) -> Result<()> {
        let version = self.stored_version().await?;
        if version == LATEST_SCHEMA_VERSION {
            Ok(())
        } else {
            Err(InfrastructureError::state(format!(
                "Turso state schema is at version {version} but this cuenv writes version \
                 {LATEST_SCHEMA_VERSION}; migrate the state store before taking the lock"
            )))
        }
    }

    /// Read one record of the tenant, with retries.
    async fn read_resource(
        &self,
        tenant: &TenantKey,
        address: &ResourceAddress,
    ) -> Result<Option<ManagedResource>> {
        let mut arguments = tenant_arguments(tenant);
        arguments.push(HranaValue::text(&address.resource_type));
        arguments.push(HranaValue::text(&address.name));
        self.execute(Statement::new(SELECT_RESOURCE, arguments))
            .await?
            .rows
            .first()
            .map(|row| row_to_resource(row))
            .transpose()
    }

    /// Read the tenant's owner row, with retries.
    async fn read_owner(&self, tenant: &TenantKey) -> Result<Option<TenantOwner>> {
        let result = self
            .execute(Statement::new(SELECT_OWNER, tenant_arguments(tenant)))
            .await?;
        Ok(result.rows.first().map(|row| {
            let field = |index: usize| {
                row.get(index)
                    .and_then(HranaValue::as_text)
                    .unwrap_or("unknown")
            };
            TenantOwner {
                instance: ProjectInstance::from_stored(field(0)),
                claimed_at: strip_control_characters(field(1)),
            }
        }))
    }

    /// Whether `lock` is still the tenant's lock.
    async fn holds(&self, tenant: &TenantKey, lock: &StateLock) -> Result<bool> {
        Ok(self
            .read_lock(tenant)
            .await?
            .is_some_and(|information| information.lock_identifier == lock.lock_identifier))
    }

    /// Bring the database up to `migrations`, applying each newer step in
    /// its own transaction.
    async fn migrate_to(&self, migrations: &[Migration]) -> Result<()> {
        let supported = migrations.last().map_or(0, |migration| migration.version);
        let current = self.schema_version().await?;
        if current > supported {
            return Err(newer_schema(current, supported));
        }
        for migration in migrations
            .iter()
            .filter(|migration| migration.version > current)
        {
            match self.apply_waiting(migration).await? {
                MigrationOutcome::Applied => {
                    tracing::info!(
                        version = migration.version,
                        "applied state schema migration"
                    );
                }
                MigrationOutcome::LockHeld => {
                    // Name the locks, so the one a dead run left behind can
                    // be found and released; the list is only a courtesy.
                    let locks = self.locks().await.unwrap_or_default();
                    return Err(InfrastructureError::StateMigrationBlocked {
                        version: migration.version,
                        locks,
                    });
                }
                MigrationOutcome::AlreadyApplied => {
                    // Another process applied this migration first; its
                    // transaction also recorded the version.
                    let recorded = self.schema_version().await?;
                    if recorded < migration.version {
                        return Err(InfrastructureError::StateSchemaConflict {
                            problem: format!(
                                "migration {} found its tables already present, but the \
                                 recorded schema version is {recorded}; the database holds \
                                 tables this cuenv did not create at this version",
                                migration.version
                            ),
                        });
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

    /// Apply one migration, waiting a bounded time for the locks that block
    /// it. While it waits, its announcement keeps new locks from being taken
    /// ([`InfrastructureError::StateMigrationPending`]), so a steady stream of
    /// short runs cannot starve it; the announcement expires by itself should
    /// this process die.
    async fn apply_waiting(&self, migration: &Migration) -> Result<MigrationOutcome> {
        let first = self.apply_migration(migration).await?;
        let wait = self.migration_wait;
        if first != MigrationOutcome::LockHeld || wait.bound.is_zero() {
            return Ok(first);
        }
        self.announce_migration(migration.version, wait).await?;
        let deadline = tokio::time::Instant::now() + wait.bound;
        let outcome = loop {
            tokio::time::sleep(wait.poll_interval).await;
            match self.apply_migration(migration).await {
                Ok(MigrationOutcome::LockHeld) if tokio::time::Instant::now() < deadline => {}
                other => break other,
            }
        };
        if let Err(error) = self.withdraw_announcement().await {
            tracing::warn!(%error, "could not withdraw the migration announcement; it expires by itself");
        }
        outcome
    }

    /// Announce that a migration to `version` waits for the locks to drain.
    async fn announce_migration(&self, version: i64, wait: MigrationWait) -> Result<()> {
        let expires_at = unix_seconds_from_now(wait.announcement_lifetime);
        self.pipeline(&[
            Statement::new(
                "DELETE FROM cuenv_infrastructure_pending_migration",
                Vec::new(),
            ),
            Statement::new(
                "INSERT INTO cuenv_infrastructure_pending_migration (version, expires_at) \
                 VALUES (?, ?)",
                vec![
                    HranaValue::integer(version),
                    HranaValue::integer(expires_at),
                ],
            ),
        ])
        .await
        .map(|_| ())
    }

    async fn withdraw_announcement(&self) -> Result<()> {
        self.execute(Statement::new(
            "DELETE FROM cuenv_infrastructure_pending_migration",
            Vec::new(),
        ))
        .await
        .map(|_| ())
    }

    /// The version of the migration waiting to run, when one announced itself
    /// and has not expired.
    async fn pending_migration(&self) -> Result<Option<i64>> {
        Ok(self
            .execute(Statement::new(
                SELECT_PENDING_MIGRATION,
                vec![HranaValue::integer(unix_seconds_from_now(Duration::ZERO))],
            ))
            .await?
            .rows
            .first()
            .and_then(|row| row.first())
            .and_then(HranaValue::as_integer))
    }

    /// Apply one migration and record its version, atomically.
    ///
    /// A migration after the first first checks, inside the transaction,
    /// that no lock row exists ([`MigrationTransaction`]), and reports
    /// [`MigrationOutcome::LockHeld`] without changing anything otherwise.
    /// It returns [`MigrationOutcome::AlreadyApplied`] when another process
    /// applied the same migration concurrently.
    async fn apply_migration(&self, migration: &Migration) -> Result<MigrationOutcome> {
        let transaction = MigrationTransaction::of(migration);
        let transaction = &transaction;
        self.retrying(move || async move {
            let result = self
                .batch_once(transaction_steps(
                    &transaction.statements,
                    transaction.commit_index,
                ))
                .await?;
            migration_outcome(&result, transaction)
        })
        .await
        .map_err(|failure| self.error(&failure))
    }

    /// Read the tenant's lock row, with retries.
    async fn read_lock(&self, tenant: &TenantKey) -> Result<Option<LockInformation>> {
        let result = self
            .execute(Statement::new(SELECT_LOCK, tenant_arguments(tenant)))
            .await?;
        Ok(result.rows.first().map(|row| {
            // Anyone with the token can write these; they are displayed.
            let field = |index: usize| {
                strip_control_characters(
                    row.get(index)
                        .and_then(HranaValue::as_text)
                        .unwrap_or("unknown"),
                )
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

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// The tenant's key columns: module, project, environment (empty without
/// `--env`).
fn tenant_arguments(tenant: &TenantKey) -> Vec<HranaValue> {
    vec![
        HranaValue::text(tenant.module_path()),
        HranaValue::text(tenant.project()),
        HranaValue::text(tenant.environment().unwrap_or(NO_ENVIRONMENT)),
    ]
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

#[async_trait]
impl StateStore for TursoStateStore {
    fn recovery_identity(&self) -> Option<String> {
        use sha2::Digest;
        let mut digest = sha2::Sha256::new();
        digest.update(b"cuenv-infrastructure-turso-recovery-v1\0");
        digest.update(backend_endpoint(&self.pipeline_url).as_str().as_bytes());
        Some(format!("{:x}", digest.finalize()))
    }

    #[tracing::instrument(skip_all)]
    async fn migrate(&self) -> Result<()> {
        self.migrate_to(MIGRATIONS).await
    }

    #[tracing::instrument(skip_all, fields(tenant = %tenant))]
    async fn list(&self, tenant: &TenantKey) -> Result<Vec<ManagedResource>> {
        if self.stored_version().await? == 0 {
            // Never migrated: nothing has been recorded yet.
            return Ok(Vec::new());
        }
        let result = self
            .execute(Statement::new(SELECT_RESOURCES, tenant_arguments(tenant)))
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
        let columns = RecordColumns::of(resource)?;
        let mut arguments = columns.insert_arguments(tenant, resource);
        arguments.extend(lock_arguments(tenant, lock));
        // The row is written only while this run still holds the lock; the
        // check and the write are one statement, so they are atomic.
        let written = self
            .execute(Statement::carrying_state(
                format!(
                    "{INSERT_RESOURCE} WHERE {LOCK_HELD} \
                     ON CONFLICT ({RESOURCE_KEY}) DO UPDATE SET \
                     provider = excluded.provider, provider_source = excluded.provider_source, \
                     schema_version = excluded.schema_version, state_json = excluded.state_json, \
                     private = excluded.private, dependencies_json = excluded.dependencies_json, \
                     tainted = excluded.tainted, identity_json = excluded.identity_json, \
                     serial = cuenv_infrastructure_resources.serial + 1, \
                     updated_at = excluded.updated_at"
                ),
                arguments,
            ))
            .await?;
        if written.affected_row_count == 0 {
            return Err(lock_lost(tenant, lock));
        }
        Ok(())
    }

    #[tracing::instrument(
        skip_all,
        fields(tenant = %tenant, address = %put.resource.address, expected = %put.expected)
    )]
    async fn put_if_unchanged(
        &self,
        tenant: &TenantKey,
        lock: &StateLock,
        put: &ConditionalPut<'_>,
    ) -> Result<()> {
        let resource = put.resource;
        let mut columns = RecordColumns::of(resource)?;
        if !resource.generation.is_nil() {
            columns.insertion_generation = resource.generation;
        }
        let statement = match put.expected {
            RecordVersion::Absent => {
                let mut arguments = columns.insert_arguments(tenant, resource);
                arguments.extend(lock_arguments(tenant, lock));
                Statement::carrying_state(
                    format!(
                        "{INSERT_RESOURCE} WHERE {LOCK_HELD} ON CONFLICT ({RESOURCE_KEY}) DO NOTHING"
                    ),
                    arguments,
                )
            }
            RecordVersion::Generation { generation, serial } => {
                let mut arguments = columns.update_arguments(resource);
                arguments.extend(tenant_arguments(tenant));
                arguments.push(HranaValue::text(&resource.address.resource_type));
                arguments.push(HranaValue::text(&resource.address.name));
                arguments.push(HranaValue::integer(serial));
                arguments.push(HranaValue::text(&generation.to_string()));
                arguments.extend(lock_arguments(tenant, lock));
                Statement::carrying_state(
                    format!(
                        "UPDATE cuenv_infrastructure_resources SET provider = ?, \
                         provider_source = ?, schema_version = ?, state_json = ?, private = ?, \
                         dependencies_json = ?, tainted = ?, identity_json = ?, \
                         serial = serial + 1, updated_at = ? \
                         WHERE module_path = ? AND project = ? AND environment = ? \
                         AND resource_type = ? AND resource_name = ? AND serial = ? \
                         AND generation = ? AND {LOCK_HELD}"
                    ),
                    arguments,
                )
            }
        };
        if self.execute(statement).await?.affected_row_count > 0 {
            return Ok(());
        }
        if !self.holds(tenant, lock).await? {
            return Err(lock_lost(tenant, lock));
        }
        let current = self.read_resource(tenant, &resource.address).await?;
        let found = RecordVersion::of(current.as_ref());
        // A retried attempt finds the write of an earlier attempt whose
        // response was lost: that is this write, already done.
        if current.is_some_and(|current| put.is_recorded(&current)) {
            return Ok(());
        }
        Err(InfrastructureError::StateChanged {
            address: resource.address.to_string(),
            expected: put.expected.to_string(),
            found: found.to_string(),
            file: None,
        })
    }

    #[tracing::instrument(skip_all, fields(tenant = %tenant, address = %address))]
    async fn delete(
        &self,
        tenant: &TenantKey,
        lock: &StateLock,
        address: &ResourceAddress,
    ) -> Result<()> {
        let mut arguments = tenant_arguments(tenant);
        arguments.push(HranaValue::text(&address.resource_type));
        arguments.push(HranaValue::text(&address.name));
        arguments.extend(lock_arguments(tenant, lock));
        let deleted = self
            .execute(Statement::new(
                format!(
                    "DELETE FROM cuenv_infrastructure_resources \
                     WHERE module_path = ? AND project = ? AND environment = ? \
                     AND resource_type = ? AND resource_name = ? AND {LOCK_HELD}"
                ),
                arguments,
            ))
            .await?;
        // Nothing deleted means the row was already gone (possibly by an
        // earlier attempt of this same call) or the lock was lost; only the
        // second is an error.
        if deleted.affected_row_count == 0 && !self.holds(tenant, lock).await? {
            return Err(lock_lost(tenant, lock));
        }
        Ok(())
    }

    #[tracing::instrument(
        skip_all,
        fields(tenant = %tenant, holder = %request.holder, lock_identifier = %request.lock.lock_identifier)
    )]
    async fn acquire_lock(
        &self,
        tenant: &TenantKey,
        request: &LockRequest<'_>,
    ) -> Result<StateLock> {
        request.lock.validate()?;
        let holder = request.holder;
        // Every write needs the lock, so this is where writes fail closed on
        // a schema this build has not migrated to.
        self.require_current_schema().await?;
        let lock_identifier = request.lock.lock_identifier.clone();
        let mut arguments = tenant_arguments(tenant);
        arguments.push(HranaValue::text(&lock_identifier));
        arguments.push(HranaValue::text(holder));
        arguments.push(HranaValue::text(&now()));
        arguments.push(HranaValue::integer(unix_seconds_from_now(Duration::ZERO)));
        // The lock row is inserted only while the schema is still the one
        // checked above, in the same statement. A migration (which refuses
        // while any lock row exists) that commits between the check and the
        // insert therefore cannot be followed by a lock, and writes, of a
        // client that does not know the new shape. A migration waiting for
        // the locks to drain announces itself, and no new lock is taken
        // until it is done.
        let insert = Statement::new(
            format!(
                "INSERT INTO cuenv_infrastructure_locks \
                 ({TENANT_KEY}, lock_identifier, holder, acquired_at) \
                 SELECT ?, ?, ?, ?, ?, ? \
                 WHERE ({SELECT_SCHEMA_VERSION}) = {LATEST_SCHEMA_VERSION} \
                 AND NOT EXISTS (SELECT 1 FROM cuenv_infrastructure_pending_migration \
                                 WHERE expires_at > ?) \
                 ON CONFLICT ({TENANT_KEY}) DO NOTHING"
            ),
            arguments,
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
                    // No lock row: either the schema moved under the insert's
                    // guard (refuse), a migration is waiting for the locks to
                    // drain (refuse, the caller runs again shortly), or the
                    // lock was released between the insert and the read
                    // (try again).
                    None => {
                        self.require_current_schema().await?;
                        if let Some(version) = self.pending_migration().await? {
                            return Err(InfrastructureError::StateMigrationPending { version });
                        }
                        if !last_attempt {
                            retry += 1;
                            continue;
                        }
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
                                 release that lock by its identifier",
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
            "DELETE FROM cuenv_infrastructure_locks \
             WHERE module_path = ? AND project = ? AND environment = ? AND lock_identifier = ?",
            lock_arguments(tenant, lock),
        ))
        .await
        .map(|_| ())
    }

    #[tracing::instrument(skip_all, fields(tenant = %tenant))]
    async fn current_lock(&self, tenant: &TenantKey) -> Result<Option<LockInformation>> {
        if self.stored_version().await? == 0 {
            // Never migrated: nobody can have taken the lock.
            return Ok(None);
        }
        self.read_lock(tenant).await
    }

    #[tracing::instrument(skip_all)]
    async fn locks(&self) -> Result<Vec<TenantLock>> {
        if self.stored_version().await? == 0 {
            // Never migrated: nobody can have taken a lock.
            return Ok(Vec::new());
        }
        let result = self
            .execute(Statement::new(SELECT_ALL_LOCKS, Vec::new()))
            .await?;
        Ok(result
            .rows
            .iter()
            .map(|row| row_to_tenant_lock(row))
            .collect())
    }

    #[tracing::instrument(skip_all, fields(tenant = %tenant))]
    async fn addresses(&self, tenant: &TenantKey) -> Result<Vec<ResourceAddress>> {
        if self.stored_version().await? == 0 {
            return Ok(Vec::new());
        }
        let result = self
            .execute(Statement::new(SELECT_ADDRESSES, tenant_arguments(tenant)))
            .await?;
        result
            .rows
            .iter()
            .map(|row| {
                let text = |index: usize| row.get(index).and_then(HranaValue::as_text);
                match (text(0), text(1)) {
                    (Some(resource_type), Some(name)) => {
                        Ok(ResourceAddress::new(resource_type, name))
                    }
                    _ => Err(InfrastructureError::UndecodableRecord {
                        address: UNREADABLE_ADDRESS.to_string(),
                        problem: "the resource_type or resource_name column is missing or not \
                                  text"
                            .to_string(),
                    }),
                }
            })
            .collect()
    }

    #[tracing::instrument(skip_all, fields(tenant = %tenant, lock_identifier = %lock_identifier))]
    async fn force_unlock(&self, tenant: &TenantKey, lock_identifier: &str) -> Result<bool> {
        if self.stored_version().await? == 0 {
            return Ok(false);
        }
        let mut arguments = tenant_arguments(tenant);
        arguments.push(HranaValue::text(lock_identifier));
        let released = self
            .execute(Statement::new(
                "DELETE FROM cuenv_infrastructure_locks \
                 WHERE module_path = ? AND project = ? AND environment = ? \
                 AND lock_identifier = ?",
                arguments,
            ))
            .await?;
        Ok(released.affected_row_count > 0)
    }

    #[tracing::instrument(skip_all, fields(tenant = %tenant))]
    async fn owner(&self, tenant: &TenantKey) -> Result<Option<TenantOwner>> {
        if self.stored_version().await? == 0 {
            // Never migrated: no owner can be recorded.
            return Ok(None);
        }
        self.read_owner(tenant).await
    }

    #[tracing::instrument(skip_all, fields(tenant = %tenant, instance = %claim.instance, mode = ?claim.mode))]
    async fn claim_owner(
        &self,
        tenant: &TenantKey,
        lock: &StateLock,
        claim: &OwnerClaim<'_>,
    ) -> Result<TenantOwner> {
        let conflict = match claim.mode {
            OwnerClaimMode::IfUnowned => "DO NOTHING",
            OwnerClaimMode::Transfer => {
                "DO UPDATE SET instance = excluded.instance, claimed_at = excluded.claimed_at"
            }
        };
        let mut arguments = tenant_arguments(tenant);
        arguments.push(HranaValue::text(claim.instance.as_str()));
        arguments.push(HranaValue::text(&now()));
        arguments.extend(lock_arguments(tenant, lock));
        let written = self
            .execute(Statement::new(
                format!(
                    "INSERT INTO cuenv_infrastructure_owners ({TENANT_KEY}, instance, claimed_at) \
                     SELECT ?, ?, ?, ?, ? WHERE {LOCK_HELD} ON CONFLICT ({TENANT_KEY}) {conflict}"
                ),
                arguments,
            ))
            .await?;
        if written.affected_row_count == 0 && !self.holds(tenant, lock).await? {
            return Err(lock_lost(tenant, lock));
        }
        self.read_owner(tenant).await?.ok_or_else(|| {
            InfrastructureError::state(format!("the owner record of {tenant} was not written"))
        })
    }
}

fn lock_arguments(tenant: &TenantKey, lock: &StateLock) -> Vec<HranaValue> {
    let mut arguments = tenant_arguments(tenant);
    arguments.push(HranaValue::text(&lock.lock_identifier));
    arguments
}

/// A record's serialized columns.
struct RecordColumns {
    state_json: String,
    dependencies_json: String,
    identity_json: Option<String>,
    timestamp: String,
    insertion_generation: uuid::Uuid,
}

impl RecordColumns {
    /// Serialize `resource`'s JSON columns. Errors name the column and the
    /// failure's category, never a value.
    fn of(resource: &ManagedResource) -> Result<Self> {
        let failed = |column: &str, error: &serde_json::Error| {
            InfrastructureError::state(format!(
                "cannot serialize the {column} of {} ({})",
                resource.address,
                json_error_category(error)
            ))
        };
        Ok(Self {
            state_json: serde_json::to_string(&resource.state)
                .map_err(|error| failed("state", &error))?,
            dependencies_json: serde_json::to_string(&resource.dependencies)
                .map_err(|error| failed("dependencies", &error))?,
            identity_json: resource
                .identity
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|error| failed("identity", &error))?,
            timestamp: now(),
            insertion_generation: uuid::Uuid::new_v4(),
        })
    }

    /// Arguments of [`INSERT_RESOURCE`].
    fn insert_arguments(&self, tenant: &TenantKey, resource: &ManagedResource) -> Vec<HranaValue> {
        let mut arguments = tenant_arguments(tenant);
        arguments.push(HranaValue::text(&resource.address.resource_type));
        arguments.push(HranaValue::text(&resource.address.name));
        arguments.extend(self.update_arguments(resource));
        arguments.push(HranaValue::text(&self.timestamp));
        arguments.push(HranaValue::text(&self.insertion_generation.to_string()));
        arguments
    }

    /// Provider through `identity_json`, then the update timestamp.
    fn update_arguments(&self, resource: &ManagedResource) -> Vec<HranaValue> {
        vec![
            HranaValue::text(&resource.provider),
            HranaValue::text(&resource.provider_source),
            HranaValue::integer(resource.schema_version),
            HranaValue::text(&self.state_json),
            HranaValue::blob(&resource.private),
            HranaValue::text(&self.dependencies_json),
            HranaValue::integer(i64::from(resource.tainted)),
            HranaValue::optional_text(self.identity_json.as_deref()),
            HranaValue::text(&self.timestamp),
        ]
    }
}

fn newer_schema(found: i64, supported: i64) -> InfrastructureError {
    InfrastructureError::StateSchemaNewer { found, supported }
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

/// Decode one row of [`SELECT_ALL_LOCKS`]. The text is displayed, and anyone
/// with the token can write these rows, so control characters are removed.
fn row_to_tenant_lock(row: &[HranaValue]) -> TenantLock {
    let field = |index: usize| {
        strip_control_characters(
            row.get(index)
                .and_then(HranaValue::as_text)
                .unwrap_or("unknown"),
        )
    };
    TenantLock {
        module_path: field(0),
        project: field(1),
        environment: Some(field(2)).filter(|environment| !environment.is_empty()),
        lock: LockInformation {
            lock_identifier: field(3),
            holder: field(4),
            acquired_at: field(5),
        },
    }
}

/// The current time plus `offset`, in whole seconds since the Unix epoch.
fn unix_seconds_from_now(offset: Duration) -> i64 {
    chrono::Utc::now()
        .timestamp()
        .saturating_add(i64::try_from(offset.as_secs()).unwrap_or(i64::MAX))
}

/// What the tables named like cuenv's say about who created them.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Layout {
    /// Nothing of an earlier or foreign layout.
    Recognized,
    /// Tables only an unreleased development build created.
    Unreleased(Vec<String>),
    /// This cuenv's own table names, without the record of its migrations.
    Unrecorded(Vec<String>),
}

/// Classify the tables of a database that has no migration record of this
/// cuenv (`names`: every table whose name starts with `cuenv_infrastructure`).
/// Tables of unreleased development builds are reported first: they explain
/// why this cuenv's own names may also be present.
fn classify_layout(names: &[String]) -> Layout {
    let unreleased: Vec<String> = names
        .iter()
        .filter(|name| {
            UNRELEASED_LAYOUT_TABLES.contains(&name.as_str())
                || UNRELEASED_LAYOUT_PREFIXES
                    .iter()
                    .any(|prefix| name.starts_with(prefix))
        })
        .cloned()
        .collect();
    if !unreleased.is_empty() {
        return Layout::Unreleased(unreleased);
    }
    let unrecorded: Vec<String> = names
        .iter()
        .filter(|name| DATA_TABLES.contains(&name.as_str()))
        .cloned()
        .collect();
    if unrecorded.is_empty() {
        Layout::Recognized
    } else {
        Layout::Unrecorded(unrecorded)
    }
}

/// Stands in for the address of a row whose address columns are unreadable.
const UNREADABLE_ADDRESS: &str = "(a record with an unreadable address)";

/// Decode one row of [`SELECT_RESOURCES`].
///
/// Every failure is [`InfrastructureError::UndecodableRecord`] naming the
/// address and the column, never a value: the state store answered, so this
/// is damaged or foreign content, not a connection problem.
fn row_to_resource(row: &[HranaValue]) -> Result<ManagedResource> {
    let undecodable = |address: &str, problem: String| InfrastructureError::UndecodableRecord {
        address: strip_control_characters(address),
        problem,
    };
    let column_text = |index: usize, name: &str| {
        row.get(index)
            .and_then(HranaValue::as_text)
            .map(str::to_string)
            .ok_or_else(|| format!("the {name} column is missing or not text"))
    };
    let address_text = match (
        column_text(0, "resource_type"),
        column_text(1, "resource_name"),
    ) {
        (Ok(resource_type), Ok(name)) => ResourceAddress::new(resource_type, name),
        (Err(problem), _) | (_, Err(problem)) => {
            return Err(undecodable(UNREADABLE_ADDRESS, problem));
        }
    };
    let address = address_text.to_string();
    let integer = |index: usize, name: &str| {
        row.get(index)
            .and_then(HranaValue::as_integer)
            .ok_or_else(|| {
                undecodable(
                    &address,
                    format!("the {name} column is missing or not a number"),
                )
            })
    };
    let text = |index: usize, name: &str| {
        column_text(index, name).map_err(|problem| undecodable(&address, problem))
    };
    // serde messages can quote the value they failed on: report the
    // column, the category and the position only.
    let corrupt = |column: &str, error: &serde_json::Error| {
        undecodable(
            &address,
            format!(
                "{column} is not valid JSON ({})",
                describe_json_error(error)
            ),
        )
    };
    let state_json = text(5, "state_json")?;
    let dependencies_json = text(7, "dependencies_json")?;
    let identity = row
        .get(9)
        .and_then(HranaValue::as_text)
        .map(serde_json::from_str)
        .transpose()
        .map_err(|error| corrupt("identity_json", &error))?;
    let generation = text(11, "generation")?;
    Ok(ManagedResource {
        provider: text(2, "provider")?,
        provider_source: text(3, "provider_source")?,
        schema_version: integer(4, "schema_version")?,
        state: serde_json::from_str(&state_json).map_err(|error| corrupt("state_json", &error))?,
        private: row
            .get(6)
            .map(HranaValue::as_blob)
            .transpose()
            .map_err(|_| undecodable(&address, "the private column is not a blob".to_string()))?
            .flatten()
            .unwrap_or_default(),
        dependencies: serde_json::from_str(&dependencies_json)
            .map_err(|error| corrupt("dependencies_json", &error))?,
        tainted: integer(8, "tainted")? != 0,
        identity,
        serial: integer(10, "serial")?,
        generation: uuid::Uuid::parse_str(&generation)
            .ok()
            .filter(|generation| !generation.is_nil())
            .ok_or_else(|| {
                undecodable(
                    &address,
                    "the generation column is not an insertion identifier".to_string(),
                )
            })?,
        address: address_text,
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
    Failure {
        kind: if busy {
            FailureKind::Transient
        } else {
            FailureKind::Statement
        },
        message: match disclosure {
            Disclosure::Full => format!(
                "Turso statement failed: {} ({code})",
                strip_control_characters(&error.message)
            ),
            Disclosure::CodeOnly => format!(
                "Turso statement failed with {code} (message withheld: the statement carried \
                 resource state)"
            ),
        },
    }
}

/// How much of a failed request's response an error may quote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Disclosure {
    /// The request carried no resource state; messages may be quoted.
    #[default]
    Full,
    /// The request carried resource state, which the server may echo back:
    /// quote only the HTTP status and error code.
    CodeOnly,
}

impl Disclosure {
    fn of(statements: &[Statement]) -> Self {
        if statements
            .iter()
            .all(|statement| statement.disclosure == Self::Full)
        {
            Self::Full
        } else {
            Self::CodeOnly
        }
    }
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
            strip_control_characters(&describe_error_body(error.body, error.total_bytes))
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

/// What SQLite says when the fence's `abs` of the smallest integer overflows.
const FENCE_ERROR: &str = "integer overflow";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MigrationOutcome {
    Applied,
    /// The migration changed nothing because a lock row exists.
    LockHeld,
    /// The migration failed because its tables or columns already exist:
    /// another process applied it concurrently.
    AlreadyApplied,
}

/// The statements of one migration's transaction: `BEGIN`, the lock fence of
/// a migration after the first, the migration's own statements, the version
/// bookkeeping, `COMMIT` at `commit_index`, then `ROLLBACK`.
struct MigrationTransaction {
    statements: Vec<Statement>,
    /// The statement that fails while any lock row exists; `None` for the
    /// first migration, which creates the lock table.
    fence_index: Option<usize>,
    commit_index: usize,
}

impl MigrationTransaction {
    fn of(migration: &Migration) -> Self {
        let version_arguments = || vec![HranaValue::integer(migration.version)];
        let mut statements = vec![Statement::new("BEGIN IMMEDIATE", Vec::new())];
        let mut fence_index = None;
        if migration.version > INITIAL_SCHEMA_VERSION {
            // A run that holds a lock may be between two writes. Moving the
            // schema under it would land its next write in a shape it does
            // not know, so the migration refuses while any lock row exists.
            // The check is inside the transaction: with `BEGIN IMMEDIATE`
            // holding the write lock, no run can take a lock after it and
            // before the commit.
            //
            // SQLite has no statement that raises an error on a condition,
            // and a server like `sqld` rejects temporary tables, triggers
            // and `RAISE`. `abs` of the smallest integer is an "integer
            // overflow" error, though, and the argument is that integer
            // exactly when at least one lock row exists: the capped count
            // (0 or 1) is subtracted from -(2^63 - 1). The count keeps the
            // expression from being folded into a constant that would fail
            // whether or not a lock exists.
            fence_index = Some(statements.len());
            statements.push(Statement::new(
                "SELECT abs(-9223372036854775807 - MIN(COUNT(*), 1)) \
                 FROM cuenv_infrastructure_locks",
                Vec::new(),
            ));
        }
        statements.extend(
            migration
                .statements
                .iter()
                .map(|sql| Statement::new(*sql, Vec::new())),
        );
        // The recorded version only ever moves forward, even if a slower
        // migrator finishes an older step after a faster one.
        statements.extend([
            Statement::new(
                "DELETE FROM cuenv_infrastructure_migrations WHERE version < ?",
                version_arguments(),
            ),
            Statement::new(
                "INSERT INTO cuenv_infrastructure_migrations (version) SELECT ?1 \
                 WHERE NOT EXISTS (SELECT 1 FROM cuenv_infrastructure_migrations WHERE version >= ?1)",
                version_arguments(),
            ),
            Statement::new("COMMIT", Vec::new()),
            Statement::new("ROLLBACK", Vec::new()),
        ]);
        let commit_index = statements.len() - 2;
        Self {
            statements,
            fence_index,
            commit_index,
        }
    }
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

fn migration_outcome(
    result: &BatchResult,
    transaction: &MigrationTransaction,
) -> Attempted<MigrationOutcome> {
    let first_error = result
        .step_errors
        .iter()
        .take(transaction.commit_index + 1)
        .enumerate()
        .find_map(|(index, error)| error.as_ref().map(|error| (index, error)));
    if let Some((index, error)) = first_error {
        // The fence fails with an integer overflow when a lock row exists;
        // any other failure of that statement (the lock table missing, the
        // database refusing it) is a failure of the migration, not a held
        // lock.
        if transaction.fence_index == Some(index) && error.message.contains(FENCE_ERROR) {
            return Ok(MigrationOutcome::LockHeld);
        }
        if error.message.contains("duplicate column name")
            || error.message.contains("already exists")
        {
            return Ok(MigrationOutcome::AlreadyApplied);
        }
        return Err(statement_failure(error, Disclosure::Full));
    }
    if result
        .step_results
        .get(transaction.commit_index)
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
    /// What errors about this statement may quote; never sent.
    #[serde(skip)]
    disclosure: Disclosure,
}

impl Statement {
    fn new(sql: impl Into<String>, arguments: Vec<HranaValue>) -> Self {
        Self {
            sql: sql.into(),
            arguments,
            want_rows: true,
            disclosure: Disclosure::Full,
        }
    }

    /// A statement whose arguments include resource state: errors about it
    /// never quote the server's messages.
    fn carrying_state(sql: impl Into<String>, arguments: Vec<HranaValue>) -> Self {
        Self {
            disclosure: Disclosure::CodeOnly,
            ..Self::new(sql, arguments)
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
                    // The decoder's message quotes the offending byte.
                    .map_err(|_| {
                        InfrastructureError::state("corrupt private blob: not valid base64")
                    })
            }
            _ => Ok(None),
        }
    }
}

#[cfg(test)]
mod tests;
