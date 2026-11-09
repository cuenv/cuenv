//! The SQLite state store, independent of how statements reach the database.
//!
//! Turso (libSQL over Hrana) and Cloudflare D1 both hold SQLite databases
//! reached over HTTP. Everything that decides what is stored, and how a write
//! is fenced, lives here once: the schema and its migrations, the lock-fenced
//! writes, compare and swap, last-writer recording, row decoding and retries.
//! [`SqlTransport`] is all a backend supplies: sending statements, running
//! them as one transaction, and the spelling of the few parameters a backend
//! cannot bind directly.
//!
//! One table family holds every identity: resources, locks and last writers
//! (the `cuenv_infrastructure_owners` table, named for the retired ownership
//! gate) are keyed
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

mod implementation;
pub(super) mod legacy_migration;
#[cfg(test)]
pub(super) mod tests;

pub use implementation::MIGRATION_FENCE_PREFIX;
pub(super) use implementation::{
    Attempted, Dialect, Disclosure, ExecuteResult, Failure, FailureKind, SqlStateStore,
    SqlTransport, SqlValue, Statement, StatementError, TransactionResult, row_to_resource,
    sql_state_store,
};
#[cfg(test)]
pub(super) use implementation::{
    CREATE_SCHEMA_TABLE, INITIAL_SCHEMA_VERSION, LATEST_SCHEMA_VERSION, Layout, MIGRATIONS,
    Migration, MigrationOutcome, MigrationTransaction, MigrationWait, RetryPolicy, SCHEMA_TABLE,
    SELECT_ADDRESSES, SELECT_ALL_LOCKS, SELECT_RESOURCES, SELECT_SCHEMA_VERSION, classify_layout,
    migration_outcome, tenant_arguments,
};
