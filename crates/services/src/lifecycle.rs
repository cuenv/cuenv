//! Service lifecycle state machine.
//!
//! Enforces valid state transitions for service supervision.

use serde::{Deserialize, Serialize};
use std::fmt;

use crate::Error;

/// Lifecycle states for a supervised service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ServiceLifecycle {
    /// Waiting for dependencies to be satisfied.
    Pending,
    /// Process spawned, awaiting readiness probe.
    Starting,
    /// Readiness probe passed; downstream nodes may start.
    Ready,
    /// Restart policy triggered; backoff timer running.
    Restarting,
    /// Shutdown signal sent, awaiting process exit.
    Stopping,
    /// Process exited cleanly or torn down by user.
    Stopped,
    /// Exceeded max restarts, readiness timed out, or unrecoverable error.
    Failed,
}

impl ServiceLifecycle {
    /// Attempt a state transition, returning an error if invalid.
    ///
    /// Valid transitions:
    /// - Pending -> Starting
    /// - Starting -> Ready | Failed
    /// - Ready -> Restarting | Stopping
    /// - Restarting -> Starting
    /// - Stopping -> Stopped
    pub fn transition(self, to: Self, service_name: &str) -> crate::Result<Self> {
        let valid = matches!(
            (self, to),
            (Self::Pending, Self::Starting)
                | (Self::Starting, Self::Ready)
                | (Self::Starting, Self::Failed)
                | (Self::Ready, Self::Restarting)
                | (Self::Ready, Self::Stopping)
                | (Self::Restarting, Self::Starting)
                | (Self::Stopping, Self::Stopped)
                // Allow direct transitions for crash scenarios
                | (Self::Starting, Self::Restarting)
                | (Self::Starting, Self::Stopping)
                | (Self::Restarting, Self::Failed)
                | (Self::Restarting, Self::Stopping)
        );

        if valid {
            Ok(to)
        } else {
            Err(Error::InvalidTransition {
                name: service_name.to_string(),
                from: self.to_string(),
                to: to.to_string(),
            })
        }
    }

    /// Whether this state represents a terminal condition.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Stopped | Self::Failed)
    }

    /// Whether the service is considered alive (not terminal, not pending).
    #[must_use]
    pub const fn is_alive(self) -> bool {
        matches!(
            self,
            Self::Starting | Self::Ready | Self::Restarting | Self::Stopping
        )
    }
}

impl fmt::Display for ServiceLifecycle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Starting => write!(f, "starting"),
            Self::Ready => write!(f, "ready"),
            Self::Restarting => write!(f, "restarting"),
            Self::Stopping => write!(f, "stopping"),
            Self::Stopped => write!(f, "stopped"),
            Self::Failed => write!(f, "failed"),
        }
    }
}

impl Default for ServiceLifecycle {
    fn default() -> Self {
        Self::Pending
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_transitions() {
        let cases = vec![
            (ServiceLifecycle::Pending, ServiceLifecycle::Starting),
            (ServiceLifecycle::Starting, ServiceLifecycle::Ready),
            (ServiceLifecycle::Starting, ServiceLifecycle::Failed),
            (ServiceLifecycle::Ready, ServiceLifecycle::Restarting),
            (ServiceLifecycle::Ready, ServiceLifecycle::Stopping),
            (ServiceLifecycle::Restarting, ServiceLifecycle::Starting),
            (ServiceLifecycle::Stopping, ServiceLifecycle::Stopped),
            (ServiceLifecycle::Starting, ServiceLifecycle::Restarting),
            (ServiceLifecycle::Starting, ServiceLifecycle::Stopping),
            (ServiceLifecycle::Restarting, ServiceLifecycle::Failed),
            (ServiceLifecycle::Restarting, ServiceLifecycle::Stopping),
        ];

        for (from, to) in cases {
            assert!(
                from.transition(to, "test").is_ok(),
                "Expected {from} -> {to} to be valid"
            );
        }
    }

    #[test]
    fn test_invalid_transitions() {
        let cases = vec![
            (ServiceLifecycle::Pending, ServiceLifecycle::Ready),
            (ServiceLifecycle::Pending, ServiceLifecycle::Stopped),
            (ServiceLifecycle::Ready, ServiceLifecycle::Pending),
            (ServiceLifecycle::Stopped, ServiceLifecycle::Starting),
            (ServiceLifecycle::Failed, ServiceLifecycle::Starting),
        ];

        for (from, to) in cases {
            assert!(
                from.transition(to, "test").is_err(),
                "Expected {from} -> {to} to be invalid"
            );
        }
    }

    #[test]
    fn test_terminal_states() {
        assert!(ServiceLifecycle::Stopped.is_terminal());
        assert!(ServiceLifecycle::Failed.is_terminal());
        assert!(!ServiceLifecycle::Ready.is_terminal());
        assert!(!ServiceLifecycle::Pending.is_terminal());
    }

    #[test]
    fn test_alive_states() {
        assert!(ServiceLifecycle::Starting.is_alive());
        assert!(ServiceLifecycle::Ready.is_alive());
        assert!(ServiceLifecycle::Restarting.is_alive());
        assert!(ServiceLifecycle::Stopping.is_alive());
        assert!(!ServiceLifecycle::Pending.is_alive());
        assert!(!ServiceLifecycle::Stopped.is_alive());
        assert!(!ServiceLifecycle::Failed.is_alive());
    }

    #[test]
    fn test_display() {
        assert_eq!(ServiceLifecycle::Pending.to_string(), "pending");
        assert_eq!(ServiceLifecycle::Ready.to_string(), "ready");
        assert_eq!(ServiceLifecycle::Failed.to_string(), "failed");
    }

    #[test]
    fn test_default() {
        assert_eq!(ServiceLifecycle::default(), ServiceLifecycle::Pending);
    }

    #[test]
    fn test_serde_roundtrip() {
        let states = vec![
            ServiceLifecycle::Pending,
            ServiceLifecycle::Starting,
            ServiceLifecycle::Ready,
            ServiceLifecycle::Restarting,
            ServiceLifecycle::Stopping,
            ServiceLifecycle::Stopped,
            ServiceLifecycle::Failed,
        ];
        for state in states {
            let json = serde_json::to_string(&state).unwrap();
            let parsed: ServiceLifecycle = serde_json::from_str(&json).unwrap();
            assert_eq!(state, parsed);
        }
    }
}
