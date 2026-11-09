//! Service supervision, readiness probes, and process management for cuenv.
//!
//! This crate provides the runtime behavior for long-running supervised
//! processes defined via `#Service` in CUE configuration. It implements:
//!
//! - Lifecycle state machine (Pending -> Starting -> Ready -> Stopped/Failed)
//! - Readiness probes (port, HTTP, log pattern, command, delay)
//! - Restart supervision with exponential backoff
//! - File watching for restart-on-change
//! - Session state management under `.cuenv/run/`
//! - Persisted manual service control requests
//! - Top-level service controller for `cuenv up` orchestration

pub(crate) mod control;
pub mod controller;
pub mod duration;
pub mod lifecycle;
pub mod probes;
pub(crate) mod process;
pub mod session;
pub mod supervisor;
pub mod watcher;

mod error;

pub use error::{Error, Result};
