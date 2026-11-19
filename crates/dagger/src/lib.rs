//! Dagger 1.0 execution backend and Dang workspace emitter for cuenv.
//!
//! This crate provides two complementary integrations with Dagger 1.0:
//!
//! 1. **Direct Dagger Engine GraphQL backend** ([`DaggerSession`],
//!    [`DaggerClient`], [`DaggerTaskBackend`]): connects to a local or remote
//!    Dagger Engine (`local`, `cloud`, or custom URI) via `dagger session` or
//!    an existing `DAGGER_SESSION_PORT`/`DAGGER_SESSION_TOKEN` session, and
//!    executes cuenv tasks with `#ContainerRuntime` or `#DaggerRuntime`
//!    directly in containers with Dagger-native caching, `CacheVolume`s,
//!    secrets, and `#Service` bindings.
//! 2. **Dagger 1.0 workspace & Dang DSL emitter** ([`DaggerWorkspaceEmitter`]):
//!    generates `dagger.toml`, `.dagger/modules/<name>/dagger-module.toml`, and
//!    `.dagger/modules/<name>/main.dang` (with `@check` functions and `@up`
//!    services) for `cuenv sync ci` and `cuenv ci --export dagger`.

pub mod client;
pub mod config;
pub mod emitter;
pub mod error;
pub mod runner;
pub mod session;

pub use client::{DaggerClient, ExecOutput};
pub use config::{BackendSelection, DaggerCiConfig, EngineTarget};
pub use emitter::{DaggerWorkspaceEmitter, EmittedFile, WorkspaceEmitInput};
pub use error::{DaggerError, Result};
pub use runner::{DaggerBackendConfig, DaggerTaskBackend};
pub use session::{DaggerSession, SessionConfig, SessionEndpoint};
