use serde::{Deserialize, Serialize};
use std::fmt;

/// Identifies the package manager in use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PackageManager {
    /// npm package manager
    Npm,
    /// Bun package manager
    Bun,
    /// pnpm package manager
    Pnpm,
    /// Yarn Classic (v1.x)
    YarnClassic,
    /// Yarn Modern (v2+, Berry)
    YarnModern,
    /// Cargo (Rust)
    Cargo,
    /// Deno
    Deno,
}

impl PackageManager {
    /// Returns the lockfile name for this package manager.
    ///
    /// # Example
    ///
    /// ```
    /// use cuenv_workspaces::PackageManager;
    ///
    /// assert_eq!(PackageManager::Npm.lockfile_name(), "package-lock.json");
    /// assert_eq!(PackageManager::Cargo.lockfile_name(), "Cargo.lock");
    /// assert_eq!(PackageManager::Pnpm.lockfile_name(), "pnpm-lock.yaml");
    /// ```
    #[must_use]
    pub const fn lockfile_name(&self) -> &str {
        match self {
            Self::Npm => "package-lock.json",
            Self::Bun => "bun.lock",
            Self::Pnpm => "pnpm-lock.yaml",
            Self::YarnClassic | Self::YarnModern => "yarn.lock",
            Self::Cargo => "Cargo.lock",
            Self::Deno => "deno.lock",
        }
    }

    /// Returns the manifest file name for this package manager.
    ///
    /// # Example
    ///
    /// ```
    /// use cuenv_workspaces::PackageManager;
    ///
    /// assert_eq!(PackageManager::Npm.manifest_name(), "package.json");
    /// assert_eq!(PackageManager::Cargo.manifest_name(), "Cargo.toml");
    /// ```
    #[must_use]
    pub const fn manifest_name(&self) -> &str {
        match self {
            Self::Npm | Self::Bun | Self::Pnpm | Self::YarnClassic | Self::YarnModern => {
                "package.json"
            }
            Self::Cargo => "Cargo.toml",
            Self::Deno => "deno.json",
        }
    }

    /// Returns the workspace configuration file name for this package manager.
    ///
    /// # Example
    ///
    /// ```
    /// use cuenv_workspaces::PackageManager;
    ///
    /// assert_eq!(PackageManager::Npm.workspace_config_name(), "package.json");
    /// assert_eq!(PackageManager::Cargo.workspace_config_name(), "Cargo.toml");
    /// assert_eq!(PackageManager::Pnpm.workspace_config_name(), "pnpm-workspace.yaml");
    /// ```
    #[must_use]
    pub const fn workspace_config_name(&self) -> &str {
        match self {
            Self::Npm | Self::Bun | Self::YarnClassic | Self::YarnModern => "package.json",
            Self::Pnpm => "pnpm-workspace.yaml",
            Self::Cargo => "Cargo.toml",
            Self::Deno => "deno.json",
        }
    }
}

impl fmt::Display for PackageManager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Npm => write!(f, "npm"),
            Self::Bun => write!(f, "bun"),
            Self::Pnpm => write!(f, "pnpm"),
            Self::YarnClassic => write!(f, "yarn-classic"),
            Self::YarnModern => write!(f, "yarn-modern"),
            Self::Cargo => write!(f, "cargo"),
            Self::Deno => write!(f, "deno"),
        }
    }
}
