//! Host-side integration contracts.
//!
//! These modules define policy data only. They do not load, execute, or
//! transport plugin code.

pub mod backends;
pub mod capabilities;
pub mod cuenv;

pub use backends::{
    BackendCapabilities, BackendError, BackendKind, EnvironmentPolicy, InMemorySessionBackend,
    SessionBackend, SessionHandle, SessionSpec, unavailable,
};
