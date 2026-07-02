//! Leaf DTO crate for cuenv configuration types.
//!
//! `cuenv-manifest` holds the serde data-transfer types deserialized from
//! CUE evaluation: configuration, environment values, secret definitions,
//! and manifest structures. It deliberately contains no runtime behavior —
//! resolution, execution, and registry wiring live in `cuenv-core` and
//! above — so that any crate can depend on the types without pulling in
//! engines or providers (RFC-0006).
//!
//! Module layout mirrors the paths these types historically had in
//! `cuenv-core` (`config`, `environment`, `secrets`, `manifest`), which
//! re-exports them during the migration.

/// CI pipeline and contributor DTOs (schema/ci.cue).
pub mod ci;
pub mod config;
pub mod environment;
/// Lockfile schema types (cuenv.lock).
pub mod lockfile;
pub mod manifest;
pub mod owners;
pub mod secrets;
pub mod tasks;
pub mod tools;
