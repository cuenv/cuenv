//! Private, in-memory storage for completed hook results.

mod service;

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

pub use service::validate_snapshot;
pub(super) use service::{Broker, BrokerLifecycle, is_available, read_snapshot, remove_stale};
#[cfg(target_os = "linux")]
pub(super) use service::{BrokerNamespace, current_namespace, verify_namespace};
