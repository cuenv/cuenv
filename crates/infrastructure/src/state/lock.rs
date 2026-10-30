//! Tenant locks: the lock a run holds and what is recorded about it.

use serde::{Deserialize, Serialize};

use crate::error::{InfrastructureError, Result};
use crate::tenant::{ProjectInstance, TenantKey};

/// An acquired state lock. Every write must present it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateLock {
    /// Lock identifier, needed to write and to release it.
    pub lock_identifier: String,
}

impl StateLock {
    /// A lock with a fresh, random identifier, not yet acquired.
    ///
    /// Generating the identifier before acquiring it lets a caller register
    /// the lock it is about to take (for example, to release it from an
    /// interrupt) before the acquisition could have committed.
    #[must_use]
    pub fn generate() -> Self {
        Self {
            lock_identifier: uuid::Uuid::new_v4().to_string(),
        }
    }

    /// Check that the identifier is safe to store and display: 1 to 128
    /// letters, digits, `-` or `_`.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::Configuration`] for any other
    /// identifier.
    pub fn validate(&self) -> Result<()> {
        let identifier = &self.lock_identifier;
        let valid = (1..=128).contains(&identifier.len())
            && identifier
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || "-_".contains(character));
        if valid {
            Ok(())
        } else {
            Err(InfrastructureError::configuration(
                "a lock identifier must be 1 to 128 letters, digits, '-' or '_'",
            ))
        }
    }
}

/// What [`StateStore::acquire_lock`] needs.
#[derive(Debug, Clone, Copy)]
pub struct LockRequest<'request> {
    /// The lock to take, with its identifier already chosen
    /// ([`StateLock::generate`]).
    pub lock: &'request StateLock,
    /// Description of the holder (command, user, host, process).
    pub holder: &'request str,
    /// The CUE instance taking the lock to change state, recorded as the
    /// tenant's last writer ([`crate::state::LastWriter`]) together with the
    /// lock. `None` (unlocking, restoring, listing) leaves the recorded last
    /// writer unchanged, so a lock taken from another directory cannot
    /// replace it.
    pub instance: Option<&'request ProjectInstance>,
}

/// Who holds a tenant's lock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockInformation {
    /// Lock identifier.
    pub lock_identifier: String,
    /// Description of the holder (command, user, host, process).
    pub holder: String,
    /// When the lock was acquired (RFC 3339).
    pub acquired_at: String,
}

impl LockInformation {
    /// How long ago the lock was acquired, as short text (`45s`, `12m`,
    /// `3h 05m`, `2d 4h`), when its timestamp can be read. A lock whose
    /// timestamp lies in the future counts as just acquired.
    #[must_use]
    pub fn age_description(&self) -> Option<String> {
        let acquired = chrono::DateTime::parse_from_rfc3339(&self.acquired_at).ok()?;
        let seconds = chrono::Utc::now()
            .signed_duration_since(acquired)
            .num_seconds()
            .max(0);
        Some(match seconds {
            0..=59 => format!("{seconds}s"),
            60..=3599 => format!("{}m", seconds / 60),
            3600..=86_399 => format!("{}h {:02}m", seconds / 3600, seconds % 3600 / 60),
            _ => format!("{}d {}h", seconds / 86_400, seconds % 86_400 / 3600),
        })
    }
}

/// A lock row of any tenant, as [`StateStore::locks`] reports it: who holds
/// which project's lock, read from the database itself, without evaluating
/// the project that took it.
///
/// The names are text read back from the store, so they carry no control
/// characters; they are not validated as a [`TenantKey`] (anyone with the
/// token can write rows).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TenantLock {
    /// CUE module path of the locked tenant.
    pub module_path: String,
    /// CUE package of the locked tenant, for stores whose keys carry it;
    /// `None` for the SQL stores, whose keys do not.
    pub package: Option<String>,
    /// Project name of the locked tenant.
    pub project: String,
    /// Named environment of the locked tenant; `None` for the identity of a
    /// run without `--env`.
    pub environment: Option<String>,
    /// The lock itself.
    pub lock: LockInformation,
}

impl TenantLock {
    /// The tenant as [`TenantKey`] shows it: `module#project` and, for a
    /// named environment, `@environment`.
    #[must_use]
    pub fn label(&self) -> String {
        let mut label = format!("{}#{}", self.module_path, self.project);
        if let Some(environment) = &self.environment {
            label.push('@');
            label.push_str(environment);
        }
        label
    }

    /// The tenant key this lock belongs to, for releasing it.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::Configuration`] when the stored names
    /// are not a valid tenant (an empty project, for example).
    pub fn tenant(&self) -> Result<TenantKey> {
        match &self.environment {
            Some(environment) => {
                TenantKey::with_environment(&self.module_path, &self.project, environment)
            }
            None => TenantKey::try_new(&self.module_path, &self.project),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_lock_identifiers_are_valid_and_others_are_checked() {
        let lock = StateLock::generate();
        lock.validate().unwrap();
        assert_ne!(lock, StateLock::generate());
        for invalid in ["", "has space", "semi;colon", &"x".repeat(129)] {
            assert!(
                StateLock {
                    lock_identifier: invalid.to_string()
                }
                .validate()
                .is_err(),
                "{invalid}"
            );
        }
    }
}
