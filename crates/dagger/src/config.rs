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

/// Provider configuration deserialized from `ci.providerConfig.dagger` (`schema/ci.cue` `#DaggerConfig`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DaggerCiConfig {
    /// Target engine: `"local"`, `"cloud"`, or a remote engine URI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub engine: Option<String>,
    /// Generated Dagger module name under `.dagger/modules/<moduleName>/` (default: `"ci"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module_name: Option<String>,
    /// Generated workspace root directory relative to repo root (default: `"."`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_dir: Option<String>,
    /// Default base image for tasks that do not declare a container or dagger runtime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_image: Option<String>,
    /// Whether to emit `@up` service functions for project services (default: `true`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emit_services: Option<bool>,
}

impl DaggerCiConfig {
    /// Effective generated module name (defaults to `"ci"`).
    #[must_use]
    pub fn module_name(&self) -> &str {
        self.module_name.as_deref().unwrap_or("ci")
    }

    /// Effective workspace directory relative to repo root (defaults to `"."`).
    #[must_use]
    pub fn workspace_dir(&self) -> &str {
        self.workspace_dir.as_deref().unwrap_or(".")
    }

    /// Effective default base image (defaults to `"alpine:3.21"`).
    #[must_use]
    pub fn default_image(&self) -> &str {
        self.default_image.as_deref().unwrap_or("alpine:3.21")
    }

    /// Whether `@up` service functions should be emitted (defaults to `true`).
    #[must_use]
    pub fn should_emit_services(&self) -> bool {
        self.emit_services.unwrap_or(true)
    }

    /// Resolved [`EngineTarget`].
    #[must_use]
    pub fn engine_target(&self) -> EngineTarget {
        self.engine.as_deref().map_or_default(EngineTarget::parse)
    }
}
