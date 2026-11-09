//! Cargo manifest reading and writing.
//!
//! This module provides functionality for reading and updating version
//! information in Cargo.toml files, specifically handling the workspace
//! version inheritance pattern.

mod cargo_manifest;
mod packages;
mod updates;

pub use cargo_manifest::CargoManifest;
