//! Error type for the infrastructure engine.

use thiserror::Error;

use crate::state::TenantLock;

/// Result alias for this crate.
pub type Result<Success> = std::result::Result<Success, InfrastructureError>;

/// Errors raised while planning or applying infrastructure.
#[derive(Debug, Error)]
pub enum InfrastructureError {
    /// Invalid `infrastructure` configuration.
    #[error("infrastructure configuration error: {0}")]
    Configuration(String),

    /// Failure encoding or decoding Terraform values.
    #[error("value codec error: {0}")]
    Codec(String),

    /// Failure launching or talking to a provider plugin process.
    #[error("provider plugin error: {0}")]
    Plugin(String),

    /// A gRPC call to a provider failed.
    #[error("provider procedure {method} failed: {status}")]
    RemoteProcedure {
        /// Procedure path.
        method: String,
        /// gRPC status returned by the provider.
        status: Box<tonic::Status>,
    },

    /// Failure downloading or installing a provider from a registry.
    #[error("provider install error: {0}")]
    Install(String),

    /// State storage failure.
    #[error("state store error: {0}")]
    State(String),

    /// The tenant's state is locked by another run.
    #[error(
        "state for {tenant} is locked by '{holder}' (lock {lock_identifier}, acquired {acquired_at}); \
         if that run is gone, release the lock"
    )]
    Locked {
        /// Tenant whose state is locked.
        tenant: String,
        /// Lock identifier.
        lock_identifier: String,
        /// Description of the lock holder.
        holder: String,
        /// When the lock was acquired.
        acquired_at: String,
    },

    /// The run no longer holds the tenant's lock, so it must not write.
    #[error(
        "state for {tenant} is no longer locked by this run (lock {lock_identifier}); \
         another run may have taken over, so nothing further was written"
    )]
    LockLost {
        /// Tenant whose lock was lost.
        tenant: String,
        /// The lock identifier this run held.
        lock_identifier: String,
    },

    /// A file (or directory) of unrecorded changes cannot be used: it is
    /// unreadable, malformed, of another format version or tenant, or not
    /// safely private to this user. Never a state store problem.
    #[error("unrecorded change file {path} cannot be used: {problem}")]
    UnrecordedFile {
        /// The file or directory.
        path: String,
        /// What is wrong with it, without its content.
        problem: String,
        /// Which kind of problem it is, so a hint names a remedy that fits.
        kind: UnrecordedFileProblem,
    },

    /// The state store's schema is newer than this build supports. Reading or
    /// writing it could misread rows whose meaning changed, so nothing is
    /// touched.
    #[error(
        "the state schema is at version {found}, newer than this cuenv supports ({supported}); \
         upgrade cuenv"
    )]
    StateSchemaNewer {
        /// Schema version recorded in the state store.
        found: i64,
        /// Newest schema version this build knows.
        supported: i64,
    },

    /// A schema migration was refused because a run holds a state lock.
    /// Migrating under a live run could land its writes in a half-migrated
    /// shape. The error lists the locks that block it, so a lock left behind
    /// by a dead run (of any project sharing the database) can be found and
    /// released.
    #[error(
        "the state schema cannot move to version {version} while a run holds a state lock: {}; \
         wait for running infrastructure commands to finish, or release a lock left behind by \
         a dead run, then run the command again",
        describe_blocking_locks(.locks)
    )]
    StateMigrationBlocked {
        /// Schema version the migration would create.
        version: i64,
        /// The locks that blocked it, oldest first (empty when they could
        /// not be read).
        locks: Vec<TenantLock>,
    },

    /// A schema migration is waiting for the running commands to finish, so
    /// no new lock can be taken until it completes.
    #[error(
        "the state schema is moving to version {version}: no command can take a state lock until \
         that finishes (it waits at most about a minute); run the command again shortly"
    )]
    StateMigrationPending {
        /// Schema version the pending migration will create.
        version: i64,
    },

    /// The database holds tables an unreleased development build of cuenv
    /// wrote. No released cuenv ever wrote that layout, so none reads it.
    #[error(
        "the state database holds tables written by an unreleased development build of cuenv \
         ({}); this cuenv does not read them",
        .tables.join(", ")
    )]
    StateUnreleasedLayout {
        /// The tables of that layout found in the database.
        tables: Vec<String>,
    },

    /// The database holds tables named like cuenv's that this cuenv did not
    /// create at this schema version (or the schema record is missing for
    /// them), so it does not use them.
    #[error("the state database cannot be used: {problem}")]
    StateSchemaConflict {
        /// What conflicts, without any row content.
        problem: String,
    },

    /// A stored record could not be decoded. The state store answered; its
    /// content is damaged or was written by something else.
    #[error("the stored record of {address} cannot be read: {problem}")]
    UndecodableRecord {
        /// Resource address, or a placeholder when the row has none.
        address: String,
        /// What is wrong, without the record's content.
        problem: String,
    },

    /// A conditional write found the stored record changed since the
    /// caller's view of it, so writing would overwrite a newer record.
    #[error(
        "{}{address} changed in the state store since this record was saved (expected \
         {expected}, found {found}); writing it would overwrite a newer record",
        saved_in(.file.as_deref())
    )]
    StateChanged {
        /// Resource address.
        address: String,
        /// The record version the caller expected.
        expected: String,
        /// The record version found.
        found: String,
        /// The unrecorded change file being recovered, when there is one.
        file: Option<String>,
    },

    /// Another CUE instance owns the tenant's state.
    #[error(
        "{tenant} is owned by the CUE instance {owner}, not {instance}; refusing to act on its \
         state. If {instance} is now the right owner, transfer ownership to it"
    )]
    OwnedByAnotherInstance {
        /// Tenant whose state is owned.
        tenant: String,
        /// The recorded owner instance.
        owner: String,
        /// The instance that tried to act.
        instance: String,
    },

    /// Input or output failure.
    #[error("{context}: {source}")]
    InputOutput {
        /// What cuenv was doing.
        context: String,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
}

/// Prefix naming the unrecorded change file an error is about.
fn saved_in(file: Option<&str>) -> String {
    file.map_or_else(String::new, |file| format!("{file}: "))
}

/// What is wrong with an unrecorded change file, for choosing the remedy a
/// hint names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnrecordedFileProblem {
    /// It was saved for another state backend (or without a backend
    /// binding): recovering it needs the operator to accept that backend.
    Backend,
    /// Its format is not the one this cuenv reads: another version, damaged
    /// JSON, or not a cuenv file at all.
    Format,
    /// An unreleased development build of cuenv wrote it; no released
    /// cuenv reads that format.
    DevelopmentBuild,
    /// Anything else: unreadable, unsafe permissions, another tenant,
    /// damaged content.
    Other,
}

/// How many blocking locks an error message lists before summarizing.
const LISTED_BLOCKING_LOCKS: usize = 5;

/// The locks blocking a migration, as text for an error message.
fn describe_blocking_locks(locks: &[TenantLock]) -> String {
    if locks.is_empty() {
        return "its holders could not be read".to_string();
    }
    let mut described: Vec<String> = locks
        .iter()
        .take(LISTED_BLOCKING_LOCKS)
        .map(|held| {
            let age = held
                .lock
                .age_description()
                .map_or_else(String::new, |age| format!(", {age} ago"));
            format!(
                "{} (lock {}, held by '{}' since {}{age})",
                held.label(),
                held.lock.lock_identifier,
                held.lock.holder,
                held.lock.acquired_at
            )
        })
        .collect();
    if locks.len() > LISTED_BLOCKING_LOCKS {
        described.push(format!("and {} more", locks.len() - LISTED_BLOCKING_LOCKS));
    }
    described.join("; ")
}

impl InfrastructureError {
    /// Build a configuration error.
    #[must_use]
    pub fn configuration(message: impl Into<String>) -> Self {
        Self::Configuration(message.into())
    }

    /// Build a codec error.
    #[must_use]
    pub fn codec(message: impl Into<String>) -> Self {
        Self::Codec(message.into())
    }

    /// Build a plugin error.
    #[must_use]
    pub fn plugin(message: impl Into<String>) -> Self {
        Self::Plugin(message.into())
    }

    /// Build an install error.
    #[must_use]
    pub fn install(message: impl Into<String>) -> Self {
        Self::Install(message.into())
    }

    /// Build a state store error.
    #[must_use]
    pub fn state(message: impl Into<String>) -> Self {
        Self::State(message.into())
    }

    /// Build an input or output error with context.
    ///
    /// Never pass a `serde_json` error's message into any error: it can
    /// quote the value it failed on. Use `json_error_category` instead.
    #[must_use]
    pub fn input_output(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::InputOutput {
            context: context.into(),
            source,
        }
    }
}

/// Name a JSON error's category without its message, which can quote the
/// value (possibly a secret) it failed on.
pub(crate) fn json_error_category(error: &serde_json::Error) -> &'static str {
    match error.classify() {
        serde_json::error::Category::Io => "input or output error",
        serde_json::error::Category::Syntax => "syntax error",
        serde_json::error::Category::Data => "data error",
        serde_json::error::Category::Eof => "unexpected end",
    }
}

/// Describe a JSON error by category and position only: its message can
/// quote the value (possibly a secret) it failed on.
pub(crate) fn describe_json_error(error: &serde_json::Error) -> String {
    format!(
        "{} at line {}, column {}",
        json_error_category(error),
        error.line(),
        error.column()
    )
}

/// Remove control characters (C0, DEL and C1) from text to display.
///
/// Text from a provider or the state store must not move the cursor,
/// rewrite earlier output or change the terminal's state.
#[must_use]
pub fn strip_control_characters(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_control())
        .collect()
}

/// [`strip_control_characters`], keeping line breaks, for multi-line text
/// such as provider diagnostics.
#[must_use]
pub fn strip_control_characters_except_newlines(text: &str) -> String {
    text.chars()
        .filter(|character| *character == '\n' || !character.is_control())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_characters_are_stripped() {
        let hostile = "ok\u{1b}[2J\u{7}\u{7f}\u{9b}31m\tend\r\nnext";
        assert_eq!(strip_control_characters(hostile), "ok[2J31mendnext");
        assert_eq!(
            strip_control_characters_except_newlines(hostile),
            "ok[2J31mend\nnext"
        );
        assert_eq!(strip_control_characters("plain text é"), "plain text é");
    }

    #[test]
    fn a_blocked_migration_names_every_lock_that_blocks_it() {
        let held = |project: &str, environment: Option<&str>| TenantLock {
            module_path: "example.com/app".into(),
            project: project.into(),
            environment: environment.map(ToString::to_string),
            lock: crate::state::LockInformation {
                lock_identifier: format!("lock-{project}"),
                holder: "apply by ci".into(),
                acquired_at: "2026-01-01T00:00:00+00:00".into(),
            },
        };
        let message = InfrastructureError::StateMigrationBlocked {
            version: 2,
            locks: vec![held("web", Some("Dev")), held("api", None)],
        }
        .to_string();
        assert!(message.contains("example.com/app#web@Dev"), "{message}");
        assert!(message.contains("lock lock-web"), "{message}");
        assert!(message.contains("held by 'apply by ci'"), "{message}");
        assert!(
            message.contains("example.com/app#api (lock lock-api"),
            "{message}"
        );
        assert!(message.contains(" ago)"), "age: {message}");
        // Many locks are summarized, not all listed.
        let many: Vec<TenantLock> = (0..8)
            .map(|index| held(&format!("p{index}"), None))
            .collect();
        let message = InfrastructureError::StateMigrationBlocked {
            version: 2,
            locks: many,
        }
        .to_string();
        assert!(message.contains("and 3 more"), "{message}");
        assert!(!message.contains("#p7"), "{message}");
        // Unreadable holders still give an actionable message.
        let message = InfrastructureError::StateMigrationBlocked {
            version: 2,
            locks: Vec::new(),
        }
        .to_string();
        assert!(message.contains("could not be read"), "{message}");
    }

    #[test]
    fn messages_describe_remedies_without_naming_commands() {
        let errors = [
            InfrastructureError::Locked {
                tenant: "tenant".into(),
                lock_identifier: "lock-1".into(),
                holder: "holder".into(),
                acquired_at: "now".into(),
            },
            InfrastructureError::StateMigrationBlocked {
                version: 2,
                locks: vec![TenantLock {
                    module_path: "example.com/app".into(),
                    project: "web".into(),
                    environment: Some("Dev".into()),
                    lock: crate::state::LockInformation {
                        lock_identifier: "lock-1".into(),
                        holder: "holder".into(),
                        acquired_at: "2026-01-01T00:00:00+00:00".into(),
                    },
                }],
            },
            InfrastructureError::StateMigrationPending { version: 2 },
            InfrastructureError::StateUnreleasedLayout {
                tables: vec!["cuenv_infrastructure_schema".into()],
            },
            InfrastructureError::StateSchemaConflict {
                problem: "it holds tables".into(),
            },
            InfrastructureError::OwnedByAnotherInstance {
                tenant: "tenant".into(),
                owner: "a".into(),
                instance: "b".into(),
            },
        ];
        for error in errors {
            let message = error.to_string();
            assert!(
                !message.contains("cuenv infrastructure") && !message.contains('`'),
                "a library error names a command line: {message}"
            );
        }
    }
}
