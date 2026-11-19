//! Configuration types for Dagger 1.0 execution and CI emission.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Target Dagger Engine for `dagger session` (`DAGGER_ENGINE`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EngineTarget {
    /// Local Dagger Engine (default).
    #[default]
    Local,
    /// Dagger Cloud remote engine (`DAGGER_ENGINE=cloud`).
    Cloud,
    /// Explicit remote engine URI (e.g. `docker-container://...`, `kube-pod://...`, `tcp://...`).
    Uri(String),
}

impl EngineTarget {
    /// Parse a raw engine specification (`"local"`, `"cloud"`, or a URI).
    #[must_use]
    pub fn parse(value: &str) -> Self {
        let trimmed = value.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("local") {
            Self::Local
        } else if trimmed.eq_ignore_ascii_case("cloud") {
            Self::Cloud
        } else {
            Self::Uri(trimmed.to_string())
        }
    }

    /// Value to set for the `DAGGER_ENGINE` environment variable, if any.
    ///
    /// Returns `None` for [`Self::Local`] so Dagger uses its default local engine.
    #[must_use]
    pub fn env_value(&self) -> Option<&str> {
        match self {
            Self::Local => None,
            Self::Cloud => Some("cloud"),
            Self::Uri(uri) => Some(uri.as_str()),
        }
    }
}

impl fmt::Display for EngineTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local => write!(f, "local"),
            Self::Cloud => write!(f, "cloud"),
            Self::Uri(uri) => write!(f, "{uri}"),
        }
    }
}

impl FromStr for EngineTarget {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self::parse(s))
    }
}

/// Which tasks a [`crate::DaggerTaskBackend`] should intercept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackendSelection {
    /// Intercept only tasks whose effective runtime is `#ContainerRuntime` or `#DaggerRuntime`.
    #[default]
    RuntimeDriven,
    /// Force every task onto the Dagger backend (using a default base image if no container/dagger runtime is set).
    ForceAll,
}

/// Provider configuration from `ci.provider.dagger` (`schema/ci.cue`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DaggerCiConfig {
    /// Installed module name (default: `"ci"`).
    pub module_name: Option<String>,
    /// Source directory relative to the Dagger workspace.
    pub module_path: Option<String>,
    /// Directory containing dagger.toml, relative to the repository.
    pub workspace_dir: Option<String>,
    /// Base image for expanded tasks with no container runtime.
    pub default_image: Option<String>,
    /// Optional prebuilt Linux cuenv executable relative to the repository.
    pub cuenv_binary: Option<String>,
    /// Dagger engine version required by the generated module.
    pub engine_version: Option<String>,
    /// Whether to make service functions available to `dagger start`.
    pub emit_services: Option<bool>,
}

impl DaggerCiConfig {
    /// Effective module name.
    #[must_use]
    pub fn module_name(&self) -> &str {
        self.module_name.as_deref().unwrap_or("ci")
    }

    /// Effective workspace directory relative to the repository root.
    #[must_use]
    pub fn workspace_dir(&self) -> &str {
        self.workspace_dir.as_deref().unwrap_or(".")
    }

    /// Effective default base image for expanded checks.
    #[must_use]
    pub fn default_image(&self) -> &str {
        self.default_image.as_deref().unwrap_or("alpine:3.21")
    }

    /// Whether service functions should be public.
    #[must_use]
    pub fn should_emit_services(&self) -> bool {
        self.emit_services.unwrap_or(true)
    }
}
