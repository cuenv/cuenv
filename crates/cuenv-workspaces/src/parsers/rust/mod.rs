//! Rust package manager lockfile parsers.
//!
//! Each parser translates a Rust package manager specific lockfile into [`LockfileEntry`](crate::LockfileEntry)
//! instances using the shared [`LockfileParser`](crate::LockfileParser) trait. The implementations are
//! gated behind fine-grained Cargo features so consumers can opt into only the parsers required for
//! their toolchain.

#[cfg(feature = "parser-cargo")]
/// Cargo lockfile parser module.
pub mod cargo;

#[cfg(feature = "parser-cargo")]
pub use cargo::CargoLockfileParser;
