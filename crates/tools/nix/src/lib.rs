//! Nix flake tool provider for cuenv.
//!
//! Provides Nix-based tool resolution using flakes. Tools are installed into
//! per-project Nix profiles (managed by the `profile` module) rather than
//! copying individual binaries, ensuring full Nix closures are available.

pub mod commands;
pub mod profile;

mod provider;

pub use provider::NixToolProvider;
