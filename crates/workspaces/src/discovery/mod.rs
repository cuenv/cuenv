//! Workspace discovery implementations for various package managers.
//!
//! This module provides implementations of the [`WorkspaceDiscovery`] trait for
//! discovering workspace configurations from:
//! - `package.json` (npm, Bun, Yarn)
//! - `pnpm-workspace.yaml` (pnpm)
//! - `Cargo.toml` (Rust/Cargo)
//!
//! # Usage
//!
//! ```rust
//! # #[cfg(any(feature = "discovery-package-json", feature = "discovery-cargo"))]
//! use cuenv_workspaces::WorkspaceDiscovery;
//! let root = tempfile::tempdir()?;
//!
//! # #[cfg(feature = "discovery-package-json")]
//! # {
//! std::fs::write(root.path().join("package.json"), r#"{"name":"demo"}"#)?;
//! let npm = cuenv_workspaces::discovery::PackageJsonDiscovery.discover(root.path())?;
//! assert_eq!(npm.member_count(), 0);
//! # }
//! # #[cfg(feature = "discovery-cargo")]
//! # {
//! std::fs::write(root.path().join("Cargo.toml"), "[workspace]\nmembers = []\n")?;
//! let cargo = cuenv_workspaces::discovery::CargoTomlDiscovery.discover(root.path())?;
//! assert_eq!(cargo.member_count(), 0);
//! # }
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod common;

#[cfg(feature = "discovery-cargo")]
pub mod cargo_toml;

#[cfg(feature = "discovery-package-json")]
pub mod package_json;

#[cfg(feature = "discovery-pnpm")]
pub mod pnpm_workspace;

#[cfg(feature = "discovery-cargo")]
pub use cargo_toml::CargoTomlDiscovery;

#[cfg(feature = "discovery-package-json")]
pub use package_json::PackageJsonDiscovery;

#[cfg(feature = "discovery-pnpm")]
pub use pnpm_workspace::PnpmWorkspaceDiscovery;

#[cfg(feature = "toml")]
pub use common::read_toml_file;
#[cfg(feature = "serde_yaml")]
pub use common::read_yaml_file;
pub use common::{read_json_file, resolve_glob_patterns};
