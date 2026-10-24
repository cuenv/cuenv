//! The SQLite state store, independent of how statements reach the database.
//!
//! Turso (libSQL over Hrana) and Cloudflare D1 both hold SQLite databases
//! reached over HTTP. Everything that decides what is stored, and how a write
//! is fenced, lives here once: the schema and its migrations, the lock-fenced
//! writes, compare and swap, ownership, row decoding and retries.
//! [`SqlTransport`] is all a backend supplies: sending statements, running
//! them as one transaction, and the spelling of the few parameters a backend
//! cannot bind directly.
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
//! Transient failures are retried with exponential backoff; every statement
//! issued here is safe to repeat.
//!
//! Every bound integer is compared only with a column (or stored into one),
//! never with a literal or another parameter: a backend that binds every
//! parameter as text (D1) relies on the column's affinity to compare it as a
//! number.

use std::future::Future;
use std::time::Duration;

use async_trait::async_trait;

use super::{
    ConditionalPut, LockInformation, LockRequest, ManagedResource, OwnerClaim, OwnerClaimMode,
    RecordVersion, ResourceAddress, StateLock, TenantLock, TenantOwner,
};
use crate::error::{
    InfrastructureError, Result, describe_json_error, json_error_category, strip_control_characters,
};
use crate::tenant::{ProjectInstance, TenantKey};

/// One schema migration: the statements that move the database to `version`.
pub(super) struct Migration {
    pub(super) version: i64,
    pub(super) statements: &'static [&'static str],
}

/// Ordered schema migrations. Never edit an existing entry; append a new one.
///
/// Version 1 is the first schema anyone is meant to use. No released cuenv
/// ever wrote an earlier layout; a database holding the tables of an
/// unreleased development build is refused by name (see
/// [`UNRELEASED_LAYOUT_TABLES`]) instead of being adopted or silently
/// ignored.
pub(super) const MIGRATIONS: &[Migration] = &[Migration {
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
pub(super) const INITIAL_SCHEMA_VERSION: i64 = 1;

/// Newest schema version this build knows.
pub(super) const LATEST_SCHEMA_VERSION: i64 = MIGRATIONS[MIGRATIONS.len() - 1].version;

/// Records the migrations applied. Unreleased development builds recorded
/// their (different) versions 1 to 5 in `cuenv_infrastructure_schema`; this
/// table has another name so those never collide with the versions this one
/// records, and a database that has only the old table is recognised as theirs.
pub(super) const SCHEMA_TABLE: &str = "cuenv_infrastructure_migrations";

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

pub(super) const CREATE_SCHEMA_TABLE: &str =
    "CREATE TABLE IF NOT EXISTS cuenv_infrastructure_migrations (version INTEGER NOT NULL)";

pub(super) const SELECT_SCHEMA_VERSION: &str =
    "SELECT COALESCE(MAX(version), 0) FROM cuenv_infrastructure_migrations";

/// Every table whose name starts like one of cuenv's (`_` also matches any
/// single character, which only widens the search).
const SELECT_CUENV_TABLES: &str = "SELECT name FROM sqlite_master WHERE type = 'table' \
     AND name LIKE 'cuenv_infrastructure%' ORDER BY name";

/// Every lock of every tenant, oldest first.
pub(super) const SELECT_ALL_LOCKS: &str = "SELECT module_path, project, environment, lock_identifier, \
     holder, acquired_at FROM cuenv_infrastructure_locks \
     ORDER BY acquired_at, module_path, project, environment";

/// The key columns of one tenant's records.
pub(super) const SELECT_ADDRESSES: &str = "SELECT resource_type, resource_name \
     FROM cuenv_infrastructure_resources \
     WHERE module_path = ? AND project = ? AND environment = ? \
     ORDER BY resource_type, resource_name";

/// The migration a waiting migrator announced, when its announcement has not
/// expired. One argument: the current time in seconds since the epoch.
const SELECT_PENDING_MIGRATION: &str = "SELECT version FROM cuenv_infrastructure_pending_migration \
     WHERE expires_at > ?";

pub(super) const SELECT_RESOURCES: &str = "SELECT resource_type, resource_name, provider, provider_source, \
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

const RESOURCE_KEY: &str = "module_path, project, environment, resource_type, resource_name";

const TENANT_KEY: &str = "module_path, project, environment";

/// How a backend spells the parameters it cannot bind as themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Dialect {
    /// Placeholder of a blob argument ([`SqlValue::Blob`], or
    /// [`SqlValue::Null`] for no bytes).
    pub(super) blob_parameter: &'static str,
    /// Placeholder of an optional text argument ([`SqlValue::Text`] or
    /// [`SqlValue::Null`]).
    pub(super) optional_text_parameter: &'static str,
}

impl Dialect {
    /// A backend that binds blobs and nulls as themselves.
    pub(super) const NATIVE: Self = Self {
        blob_parameter: "?",
        optional_text_parameter: "?",
    };

    /// Arguments: the tenant, the address, the columns of
    /// [`RecordColumns::insert_arguments`]. Callers append a lock check and a
    /// conflict clause.
    fn insert_resource(self) -> String {
        format!(
            "INSERT INTO cuenv_infrastructure_resources \
             (module_path, project, environment, resource_type, resource_name, provider, \
             provider_source, schema_version, state_json, private, dependencies_json, tainted, \
             identity_json, serial, created_at, updated_at, generation) \
             SELECT ?, ?, ?, ?, ?, ?, ?, ?, ?, {}, ?, ?, {}, 1, ?, ?, ?",
            self.blob_parameter, self.optional_text_parameter
        )
    }

    /// The columns of [`RecordColumns::update_arguments`], as assignments.
    fn update_assignments(self) -> String {
        format!(
            "provider = ?, provider_source = ?, schema_version = ?, state_json = ?, \
             private = {}, dependencies_json = ?, tainted = ?, identity_json = {}, \
             serial = serial + 1, updated_at = ?",
            self.blob_parameter, self.optional_text_parameter
        )
    }
}

/// How transient failures are retried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct RetryPolicy {
    /// Retries after the first attempt.
    pub(super) retries: u32,
    /// Delay before the first retry; doubled before each further retry.
    pub(super) initial_delay: Duration,
}

impl RetryPolicy {
    pub(super) const DEFAULT: Self = Self {
        retries: 3,
        initial_delay: Duration::from_millis(200),
    };

    /// Delay before retry number `retry` (zero based): 200 ms, 400 ms, 800 ms.
    pub(super) fn delay(self, retry: u32) -> Duration {
        self.initial_delay
            .saturating_mul(2_u32.saturating_pow(retry))
    }
}

/// How a migration that finds locks held waits for them, while keeping new
/// locks from being taken.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct MigrationWait {
    /// The longest a migration waits for the locks to be released.
    pub(super) bound: Duration,
    /// How often it looks again.
    pub(super) poll_interval: Duration,
    /// How long its announcement (which holds off new locks) stands if the
    /// migrating process dies; always longer than `bound`.
    pub(super) announcement_lifetime: Duration,
}

impl MigrationWait {
    pub(super) const DEFAULT: Self = Self {
        bound: Duration::from_secs(30),
        poll_interval: Duration::from_millis(500),
        announcement_lifetime: Duration::from_secs(60),
    };
}

/// How statements reach one kind of database.
#[async_trait]
pub(super) trait SqlTransport: Send + Sync {
    /// The backend's name in messages: `Turso`, `D1`.
    const NAME: &'static str;

    /// How this backend spells blob and optional text parameters.
    const DIALECT: Dialect;

    /// Execute statements in one request, in order, without retrying.
    ///
    /// A backend may run them in one transaction; nothing here depends on
    /// a statement's effects surviving when a later one in the same request
    /// fails.
    async fn execute_once(&self, statements: &[Statement]) -> Attempted<Vec<ExecuteResult>>;

    /// Execute statements as one transaction in one request, without
    /// retrying: all of them commit, or none does.
    ///
    /// A statement the database rejects is reported as
    /// [`TransactionResult::Failed`], not as an error, so the caller can tell
    /// which failure it was.
    async fn transaction_once(&self, statements: &[Statement]) -> Attempted<TransactionResult>;

    /// The secret every message must be redacted of.
    fn secret(&self) -> Option<&str>;

    /// The failure a rejected statement is, given its message may only be
    /// quoted as `disclosure` allows.
    fn statement_failure(&self, error: &StatementError, disclosure: Disclosure) -> Failure;
}

/// A [`crate::state::StateStore`] over a SQLite database reached by `T`.
#[derive(Clone)]
pub(super) struct SqlStateStore<T> {
    pub(super) transport: T,
    pub(super) retry_policy: RetryPolicy,
    /// How a migration waits for the locks that block it.
    pub(super) migration_wait: MigrationWait,
}

impl<T: SqlTransport> SqlStateStore<T> {
    pub(super) const fn new(transport: T) -> Self {
        Self {
            transport,
            retry_policy: RetryPolicy::DEFAULT,
            migration_wait: MigrationWait::DEFAULT,
        }
    }

    /// Run `attempt` until it succeeds, fails permanently, or retries run out.
    pub(super) async fn retrying<Output, Attempt>(
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
                    let delay = failure.delay(self.retry_policy, retry);
                    tracing::warn!(
                        attempt = retry + 1,
                        delay_milliseconds = u64::try_from(delay.as_millis()).unwrap_or(u64::MAX),
                        error = %self.redact(&failure.message),
                        "transient {} failure; retrying",
                        T::NAME
                    );
                    tokio::time::sleep(delay).await;
                    retry += 1;
                }
                Err(failure) if failure.is_transient() && retry > 0 => {
                    return Err(Failure::new(
                        failure.kind,
                        format!("{} (gave up after {} attempts)", failure.message, retry + 1),
                    ));
                }
                Err(failure) => return Err(failure),
            }
        }
    }

    /// Execute statements with retries.
    pub(super) async fn pipeline(&self, statements: &[Statement]) -> Result<Vec<ExecuteResult>> {
        self.retrying(move || self.transport.execute_once(statements))
            .await
            .map_err(|failure| self.error(&failure))
    }

    /// Execute one statement with retries.
    pub(super) async fn execute(&self, statement: Statement) -> Result<ExecuteResult> {
        self.pipeline(std::slice::from_ref(&statement))
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| InfrastructureError::state(format!("{} returned no result", T::NAME)))
    }

    /// Turn a failure into an error, never leaking the authentication token.
    pub(super) fn error(&self, failure: &Failure) -> InfrastructureError {
        InfrastructureError::state(self.redact(&failure.message))
    }

    pub(super) fn redact(&self, message: &str) -> String {
        match self.transport.secret() {
            Some(token) => message.replace(token, "<redacted>"),
            None => message.to_string(),
        }
    }

    /// The newest applied schema version; 0 for a database never migrated.
    /// Creates the schema table, so only the migration path calls it. Refuses
    /// first a database that holds tables of another layout.
    pub(super) async fn schema_version(&self) -> Result<i64> {
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
            .and_then(SqlValue::as_integer)
            .ok_or_else(|| {
                InfrastructureError::state(format!("{} returned no schema version", T::NAME))
            })
    }

    /// The tables whose names start like cuenv's, with one request.
    async fn cuenv_tables(&self) -> Result<Vec<String>> {
        Ok(self
            .execute(Statement::new(SELECT_CUENV_TABLES, Vec::new()))
            .await?
            .rows
            .iter()
            .filter_map(|row| row.first().and_then(SqlValue::as_text))
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
    pub(super) async fn stored_version(&self) -> Result<i64> {
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
            .and_then(SqlValue::as_integer)
            .ok_or_else(|| {
                InfrastructureError::state(format!("{} returned no schema version", T::NAME))
            })?;
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
                "{} state schema is at version {version} but this cuenv writes version \
                 {LATEST_SCHEMA_VERSION}; migrate the state store before taking the lock",
                T::NAME
            )))
        }
    }

    /// Read one record of the tenant, with retries.
    pub(super) async fn read_resource(
        &self,
        tenant: &TenantKey,
        address: &ResourceAddress,
    ) -> Result<Option<ManagedResource>> {
        let mut arguments = tenant_arguments(tenant);
        arguments.push(SqlValue::text(&address.resource_type));
        arguments.push(SqlValue::text(&address.name));
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
                    .and_then(SqlValue::as_text)
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
    pub(super) async fn migrate_to(&self, migrations: &[Migration]) -> Result<()> {
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
            // Literals, not parameters: a request of several statements
            // binds nothing (D1 refuses parameters there), and both values
            // are integers computed here.
            Statement::new(
                format!(
                    "INSERT INTO cuenv_infrastructure_pending_migration (version, expires_at) \
                     VALUES ({version}, {expires_at})"
                ),
                Vec::new(),
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
                vec![SqlValue::integer(unix_seconds_from_now(Duration::ZERO))],
            ))
            .await?
            .rows
            .first()
            .and_then(|row| row.first())
            .and_then(SqlValue::as_integer))
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
                .transport
                .transaction_once(&transaction.statements)
                .await?;
            migration_outcome(&result, transaction)
                .map_err(|error| self.transport.statement_failure(&error, Disclosure::Full))
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
                        .and_then(SqlValue::as_text)
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

    // -----------------------------------------------------------------------
    // The state store operations; see `crate::state::StateStore`.
    // -----------------------------------------------------------------------

    pub(super) async fn migrate(&self) -> Result<()> {
        self.migrate_to(MIGRATIONS).await
    }

    pub(super) async fn list(&self, tenant: &TenantKey) -> Result<Vec<ManagedResource>> {
        if self.stored_version().await? == 0 {
            // Never migrated: nothing has been recorded yet.
            return Ok(Vec::new());
        }
        let result = self
            .execute(Statement::new(SELECT_RESOURCES, tenant_arguments(tenant)))
            .await?;
        result.rows.iter().map(|row| row_to_resource(row)).collect()
    }

    pub(super) async fn put(
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
                    "{} WHERE {LOCK_HELD} \
                     ON CONFLICT ({RESOURCE_KEY}) DO UPDATE SET \
                     provider = excluded.provider, provider_source = excluded.provider_source, \
                     schema_version = excluded.schema_version, state_json = excluded.state_json, \
                     private = excluded.private, dependencies_json = excluded.dependencies_json, \
                     tainted = excluded.tainted, identity_json = excluded.identity_json, \
                     serial = cuenv_infrastructure_resources.serial + 1, \
                     updated_at = excluded.updated_at",
                    T::DIALECT.insert_resource()
                ),
                arguments,
            ))
            .await?;
        if written.affected_row_count == 0 {
            return Err(lock_lost(tenant, lock));
        }
        Ok(())
    }

    pub(super) async fn put_if_unchanged(
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
                        "{} WHERE {LOCK_HELD} ON CONFLICT ({RESOURCE_KEY}) DO NOTHING",
                        T::DIALECT.insert_resource()
                    ),
                    arguments,
                )
            }
            RecordVersion::Generation { generation, serial } => {
                let mut arguments = columns.update_arguments(resource);
                arguments.extend(tenant_arguments(tenant));
                arguments.push(SqlValue::text(&resource.address.resource_type));
                arguments.push(SqlValue::text(&resource.address.name));
                arguments.push(SqlValue::integer(serial));
                arguments.push(SqlValue::text(&generation.to_string()));
                arguments.extend(lock_arguments(tenant, lock));
                Statement::carrying_state(
                    format!(
                        "UPDATE cuenv_infrastructure_resources SET {} \
                         WHERE module_path = ? AND project = ? AND environment = ? \
                         AND resource_type = ? AND resource_name = ? AND serial = ? \
                         AND generation = ? AND {LOCK_HELD}",
                        T::DIALECT.update_assignments()
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

    pub(super) async fn delete(
        &self,
        tenant: &TenantKey,
        lock: &StateLock,
        address: &ResourceAddress,
    ) -> Result<()> {
        let mut arguments = tenant_arguments(tenant);
        arguments.push(SqlValue::text(&address.resource_type));
        arguments.push(SqlValue::text(&address.name));
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

    pub(super) async fn acquire_lock(
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
        arguments.push(SqlValue::text(&lock_identifier));
        arguments.push(SqlValue::text(holder));
        arguments.push(SqlValue::text(&now()));
        arguments.push(SqlValue::integer(unix_seconds_from_now(Duration::ZERO)));
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
            match self
                .transport
                .execute_once(std::slice::from_ref(&insert))
                .await
            {
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
                    let delay = failure.delay(self.retry_policy, retry);
                    tracing::warn!(
                        attempt = retry + 1,
                        error = %self.redact(&failure.message),
                        "transient {} failure acquiring the state lock; retrying",
                        T::NAME
                    );
                    tokio::time::sleep(delay).await;
                }
                Err(failure) => return Err(self.error(&failure)),
            }
            retry += 1;
        }
    }

    pub(super) async fn unlock(&self, tenant: &TenantKey, lock: &StateLock) -> Result<bool> {
        self.execute(Statement::new(
            "DELETE FROM cuenv_infrastructure_locks \
             WHERE module_path = ? AND project = ? AND environment = ? AND lock_identifier = ?",
            lock_arguments(tenant, lock),
        ))
        .await
        .map(|deleted| deleted.affected_row_count > 0)
    }

    pub(super) async fn current_lock(&self, tenant: &TenantKey) -> Result<Option<LockInformation>> {
        if self.stored_version().await? == 0 {
            // Never migrated: nobody can have taken the lock.
            return Ok(None);
        }
        self.read_lock(tenant).await
    }

    pub(super) async fn locks(&self) -> Result<Vec<TenantLock>> {
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

    pub(super) async fn addresses(&self, tenant: &TenantKey) -> Result<Vec<ResourceAddress>> {
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
                let text = |index: usize| row.get(index).and_then(SqlValue::as_text);
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

    pub(super) async fn force_unlock(
        &self,
        tenant: &TenantKey,
        lock_identifier: &str,
    ) -> Result<bool> {
        if self.stored_version().await? == 0 {
            return Ok(false);
        }
        let mut arguments = tenant_arguments(tenant);
        arguments.push(SqlValue::text(lock_identifier));
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

    pub(super) async fn owner(&self, tenant: &TenantKey) -> Result<Option<TenantOwner>> {
        if self.stored_version().await? == 0 {
            // Never migrated: no owner can be recorded.
            return Ok(None);
        }
        self.read_owner(tenant).await
    }

    pub(super) async fn claim_owner(
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
        arguments.push(SqlValue::text(claim.instance.as_str()));
        arguments.push(SqlValue::text(&now()));
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

/// Implement [`crate::state::StateStore`] for a store type whose `inner`
/// field is a [`SqlStateStore`], with its own recovery identity.
macro_rules! sql_state_store {
    ($store:ty) => {
        #[async_trait::async_trait]
        impl crate::state::StateStore for $store {
            fn recovery_identity(&self) -> Option<String> {
                Some(self.backend_identity())
            }

            #[tracing::instrument(skip_all)]
            async fn migrate(&self) -> crate::error::Result<()> {
                self.inner.migrate().await
            }

            #[tracing::instrument(skip_all, fields(tenant = %tenant))]
            async fn list(
                &self,
                tenant: &crate::tenant::TenantKey,
            ) -> crate::error::Result<Vec<crate::state::ManagedResource>> {
                self.inner.list(tenant).await
            }

            #[tracing::instrument(skip_all, fields(tenant = %tenant, address = %resource.address))]
            async fn put(
                &self,
                tenant: &crate::tenant::TenantKey,
                lock: &crate::state::StateLock,
                resource: &crate::state::ManagedResource,
            ) -> crate::error::Result<()> {
                self.inner.put(tenant, lock, resource).await
            }

            #[tracing::instrument(
                skip_all,
                fields(tenant = %tenant, address = %put.resource.address, expected = %put.expected)
            )]
            async fn put_if_unchanged(
                &self,
                tenant: &crate::tenant::TenantKey,
                lock: &crate::state::StateLock,
                put: &crate::state::ConditionalPut<'_>,
            ) -> crate::error::Result<()> {
                self.inner.put_if_unchanged(tenant, lock, put).await
            }

            #[tracing::instrument(skip_all, fields(tenant = %tenant, address = %address))]
            async fn delete(
                &self,
                tenant: &crate::tenant::TenantKey,
                lock: &crate::state::StateLock,
                address: &crate::state::ResourceAddress,
            ) -> crate::error::Result<()> {
                self.inner.delete(tenant, lock, address).await
            }

            #[tracing::instrument(
                skip_all,
                fields(
                    tenant = %tenant,
                    holder = %request.holder,
                    lock_identifier = %request.lock.lock_identifier
                )
            )]
            async fn acquire_lock(
                &self,
                tenant: &crate::tenant::TenantKey,
                request: &crate::state::LockRequest<'_>,
            ) -> crate::error::Result<crate::state::StateLock> {
                self.inner.acquire_lock(tenant, request).await
            }

            #[tracing::instrument(skip_all, fields(tenant = %tenant))]
            async fn unlock(
                &self,
                tenant: &crate::tenant::TenantKey,
                lock: &crate::state::StateLock,
            ) -> crate::error::Result<bool> {
                self.inner.unlock(tenant, lock).await
            }

            #[tracing::instrument(skip_all, fields(tenant = %tenant))]
            async fn current_lock(
                &self,
                tenant: &crate::tenant::TenantKey,
            ) -> crate::error::Result<Option<crate::state::LockInformation>> {
                self.inner.current_lock(tenant).await
            }

            #[tracing::instrument(skip_all)]
            async fn locks(&self) -> crate::error::Result<Vec<crate::state::TenantLock>> {
                self.inner.locks().await
            }

            #[tracing::instrument(skip_all, fields(tenant = %tenant))]
            async fn addresses(
                &self,
                tenant: &crate::tenant::TenantKey,
            ) -> crate::error::Result<Vec<crate::state::ResourceAddress>> {
                self.inner.addresses(tenant).await
            }

            #[tracing::instrument(skip_all, fields(tenant = %tenant, lock_identifier = %lock_identifier))]
            async fn force_unlock(
                &self,
                tenant: &crate::tenant::TenantKey,
                lock_identifier: &str,
            ) -> crate::error::Result<bool> {
                self.inner.force_unlock(tenant, lock_identifier).await
            }

            #[tracing::instrument(skip_all, fields(tenant = %tenant))]
            async fn owner(
                &self,
                tenant: &crate::tenant::TenantKey,
            ) -> crate::error::Result<Option<crate::state::TenantOwner>> {
                self.inner.owner(tenant).await
            }

            #[tracing::instrument(
                skip_all,
                fields(tenant = %tenant, instance = %claim.instance, mode = ?claim.mode)
            )]
            async fn claim_owner(
                &self,
                tenant: &crate::tenant::TenantKey,
                lock: &crate::state::StateLock,
                claim: &crate::state::OwnerClaim<'_>,
            ) -> crate::error::Result<crate::state::TenantOwner> {
                self.inner.claim_owner(tenant, lock, claim).await
            }
        }
    };
}

pub(super) use sql_state_store;

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// The tenant's key columns: module, project, environment (empty without
/// `--env`).
pub(super) fn tenant_arguments(tenant: &TenantKey) -> Vec<SqlValue> {
    vec![
        SqlValue::text(tenant.module_path()),
        SqlValue::text(tenant.project()),
        SqlValue::text(tenant.environment().unwrap_or(NO_ENVIRONMENT)),
    ]
}

fn lock_arguments(tenant: &TenantKey, lock: &StateLock) -> Vec<SqlValue> {
    let mut arguments = tenant_arguments(tenant);
    arguments.push(SqlValue::text(&lock.lock_identifier));
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

    /// Arguments of [`Dialect::insert_resource`].
    fn insert_arguments(&self, tenant: &TenantKey, resource: &ManagedResource) -> Vec<SqlValue> {
        let mut arguments = tenant_arguments(tenant);
        arguments.push(SqlValue::text(&resource.address.resource_type));
        arguments.push(SqlValue::text(&resource.address.name));
        arguments.extend(self.update_arguments(resource));
        arguments.push(SqlValue::text(&self.timestamp));
        arguments.push(SqlValue::text(&self.insertion_generation.to_string()));
        arguments
    }

    /// Provider through `identity_json`, then the update timestamp: the
    /// arguments of [`Dialect::update_assignments`].
    fn update_arguments(&self, resource: &ManagedResource) -> Vec<SqlValue> {
        vec![
            SqlValue::text(&resource.provider),
            SqlValue::text(&resource.provider_source),
            SqlValue::integer(resource.schema_version),
            SqlValue::text(&self.state_json),
            SqlValue::blob(&resource.private),
            SqlValue::text(&self.dependencies_json),
            SqlValue::integer(i64::from(resource.tainted)),
            SqlValue::optional_text(self.identity_json.as_deref()),
            SqlValue::text(&self.timestamp),
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
fn row_to_tenant_lock(row: &[SqlValue]) -> TenantLock {
    let field = |index: usize| {
        strip_control_characters(
            row.get(index)
                .and_then(SqlValue::as_text)
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
pub(super) enum Layout {
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
pub(super) fn classify_layout(names: &[String]) -> Layout {
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
pub(super) fn row_to_resource(row: &[SqlValue]) -> Result<ManagedResource> {
    let undecodable = |address: &str, problem: String| InfrastructureError::UndecodableRecord {
        address: strip_control_characters(address),
        problem,
    };
    let column_text = |index: usize, name: &str| {
        row.get(index)
            .and_then(SqlValue::as_text)
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
            .and_then(SqlValue::as_integer)
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
        .and_then(SqlValue::as_text)
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
            .map(SqlValue::as_blob)
            .transpose()
            .map_err(|()| undecodable(&address, "the private column is not a blob".to_string()))?
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
// Statements, values and results
// ---------------------------------------------------------------------------

/// A SQL value, as bound to a statement or read from a row.
///
/// Null is never bound as itself: only [`Self::Blob`] without bytes and
/// [`Self::OptionalText`] without text stand for it, and only in the
/// positions [`Dialect`] spells for them, so a backend that cannot bind null
/// (D1 binds text only) can encode both.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum SqlValue {
    /// Read from a row; never bound.
    Null,
    Integer(i64),
    /// Read from a row; never bound.
    Float(f64),
    Text(String),
    /// Bytes; bound as null when there are none.
    Blob(Vec<u8>),
    /// Bound only, at [`Dialect::optional_text_parameter`]; null for none.
    OptionalText(Option<String>),
    /// A value the backend sent in a form it could not decode (a blob that
    /// is not valid base64, for example). Never bound.
    Unreadable,
}

impl SqlValue {
    pub(super) fn text(value: &str) -> Self {
        Self::Text(value.to_string())
    }

    /// Text, or null for none.
    pub(super) fn optional_text(value: Option<&str>) -> Self {
        Self::OptionalText(value.map(str::to_string))
    }

    pub(super) const fn integer(value: i64) -> Self {
        Self::Integer(value)
    }

    /// A blob, null for no bytes.
    pub(super) fn blob(bytes: &[u8]) -> Self {
        Self::Blob(bytes.to_vec())
    }

    pub(super) fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(value) => Some(value),
            _ => None,
        }
    }

    pub(super) const fn as_integer(&self) -> Option<i64> {
        match self {
            Self::Integer(value) => Some(*value),
            _ => None,
        }
    }

    /// The bytes of a blob; `None` for any other readable value, an error for
    /// a value the backend could not decode.
    pub(super) fn as_blob(&self) -> std::result::Result<Option<Vec<u8>>, ()> {
        match self {
            Self::Blob(bytes) => Ok(Some(bytes.clone())),
            Self::Unreadable => Err(()),
            _ => Ok(None),
        }
    }
}

#[derive(Debug)]
pub(super) struct Statement {
    pub(super) sql: String,
    pub(super) arguments: Vec<SqlValue>,
    /// What errors about this statement may quote.
    pub(super) disclosure: Disclosure,
}

impl Statement {
    pub(super) fn new(sql: impl Into<String>, arguments: Vec<SqlValue>) -> Self {
        Self {
            sql: sql.into(),
            arguments,
            disclosure: Disclosure::Full,
        }
    }

    /// A statement whose arguments include resource state: errors about it
    /// never quote the server's messages.
    pub(super) fn carrying_state(sql: impl Into<String>, arguments: Vec<SqlValue>) -> Self {
        Self {
            disclosure: Disclosure::CodeOnly,
            ..Self::new(sql, arguments)
        }
    }
}

/// The rows a statement returned and the rows it changed.
#[derive(Debug, Default, Clone, PartialEq)]
pub(super) struct ExecuteResult {
    pub(super) rows: Vec<Vec<SqlValue>>,
    pub(super) affected_row_count: u64,
}

/// A statement the database rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StatementError {
    /// The database's message. Quoted only as the statement's
    /// [`Disclosure`] allows.
    pub(super) message: String,
    /// The database's error code, when it sent one.
    pub(super) code: Option<String>,
}

/// How [`SqlTransport::transaction_once`] ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TransactionResult {
    /// Every statement ran and the transaction committed.
    Committed,
    /// A statement was rejected and nothing was committed.
    Failed {
        /// The rejected statement, when the backend says which it was.
        statement: Option<usize>,
        error: StatementError,
    },
}

/// How much of a failed request's response an error may quote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum Disclosure {
    /// The request carried no resource state; messages may be quoted.
    #[default]
    Full,
    /// The request carried resource state, which the server may echo back:
    /// quote only the HTTP status and error code.
    CodeOnly,
}

impl Disclosure {
    pub(super) fn of(statements: &[Statement]) -> Self {
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

// ---------------------------------------------------------------------------
// Failure classification
// ---------------------------------------------------------------------------

pub(super) type Attempted<Output> = std::result::Result<Output, Failure>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FailureKind {
    /// Worth retrying: the request may not have reached the database, or the
    /// database was briefly unavailable. The request may also have committed.
    Transient,
    /// The database rejected a statement.
    Statement,
    /// Retrying cannot help.
    Permanent,
}

/// The longest a server's `Retry-After` is honoured before a retry.
const MAXIMUM_RETRY_AFTER: Duration = Duration::from_secs(30);

#[derive(Debug)]
pub(super) struct Failure {
    pub(super) kind: FailureKind,
    pub(super) message: String,
    /// How long the server asked to wait before trying again.
    pub(super) retry_after: Option<Duration>,
}

impl Failure {
    pub(super) fn new(kind: FailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            retry_after: None,
        }
    }

    pub(super) fn permanent(message: impl Into<String>) -> Self {
        Self::new(FailureKind::Permanent, message)
    }

    /// The delay before retry number `retry`: the policy's, or longer when
    /// the server asked for longer (up to [`MAXIMUM_RETRY_AFTER`]).
    pub(super) fn delay(&self, policy: RetryPolicy, retry: u32) -> Duration {
        let backoff = policy.delay(retry);
        self.retry_after
            .map_or(backoff, |after| after.min(MAXIMUM_RETRY_AFTER).max(backoff))
    }

    pub(super) fn is_transient(&self) -> bool {
        self.kind == FailureKind::Transient
    }
}

// ---------------------------------------------------------------------------
// Migrations
// ---------------------------------------------------------------------------

/// What SQLite says when the fence's `abs` of the smallest integer overflows.
const FENCE_ERROR: &str = "integer overflow";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MigrationOutcome {
    Applied,
    /// The migration changed nothing because a lock row exists.
    LockHeld,
    /// The migration failed because its tables or columns already exist:
    /// another process applied it concurrently.
    AlreadyApplied,
}

/// The statements of one migration's transaction: the lock fence of a
/// migration after the first, the migration's own statements, then the
/// version bookkeeping. The transport runs them as one transaction.
pub(super) struct MigrationTransaction {
    pub(super) statements: Vec<Statement>,
    /// The statement that fails while any lock row exists; `None` for the
    /// first migration, which creates the lock table.
    pub(super) fence_index: Option<usize>,
}

impl MigrationTransaction {
    pub(super) fn of(migration: &Migration) -> Self {
        let version = migration.version;
        let mut statements = Vec::new();
        let mut fence_index = None;
        if migration.version > INITIAL_SCHEMA_VERSION {
            // A run that holds a lock may be between two writes. Moving the
            // schema under it would land its next write in a shape it does
            // not know, so the migration refuses while any lock row exists.
            // The check is inside the transaction, which holds the database's
            // write lock (`BEGIN IMMEDIATE` on Turso; D1 runs one batch at a
            // time), so no run can take a lock after it and before the commit.
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
        // migrator finishes an older step after a faster one. The version is
        // a literal: a transaction binds nothing (D1 refuses parameters in a
        // request of several statements).
        statements.extend([
            Statement::new(
                format!("DELETE FROM cuenv_infrastructure_migrations WHERE version < {version}"),
                Vec::new(),
            ),
            Statement::new(
                format!(
                    "INSERT INTO cuenv_infrastructure_migrations (version) SELECT {version} \
                     WHERE NOT EXISTS (SELECT 1 FROM cuenv_infrastructure_migrations \
                                       WHERE version >= {version})"
                ),
                Vec::new(),
            ),
        ]);
        Self {
            statements,
            fence_index,
        }
    }
}

/// What a migration's transaction result means. A rejected statement that is
/// not the fence, and not a migration another process applied first, is
/// returned as the error to report.
pub(super) fn migration_outcome(
    result: &TransactionResult,
    transaction: &MigrationTransaction,
) -> std::result::Result<MigrationOutcome, StatementError> {
    match result {
        TransactionResult::Committed => Ok(MigrationOutcome::Applied),
        TransactionResult::Failed { statement, error } => {
            // The fence fails with an integer overflow when a lock row
            // exists; any other failure of that statement (the lock table
            // missing, the database refusing it) is a failure of the
            // migration, not a held lock. A backend that does not say which
            // statement failed is matched by the message alone: no statement
            // of a migration but the fence does arithmetic that can overflow.
            let is_fence = transaction.fence_index.is_some()
                && statement.is_none_or(|statement| transaction.fence_index == Some(statement));
            if is_fence && error.message.contains(FENCE_ERROR) {
                return Ok(MigrationOutcome::LockHeld);
            }
            if error.message.contains("duplicate column name")
                || error.message.contains("already exists")
            {
                return Ok(MigrationOutcome::AlreadyApplied);
            }
            Err(error.clone())
        }
    }
}
