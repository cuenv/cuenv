//! Tenant identity for multi-tenant state.
//!
//! Every state row and lock is scoped by the CUE module path (the tenant)
//! and the cuenv project name (the discriminator within the tenant). There
//! is deliberately no way to address state without both.

use std::fmt;
use std::path::Path;

use crate::error::{InfrastructureError, Result};

/// The owner of a set of managed resources.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TenantKey {
    module_path: String,
    project: String,
    environment: Option<String>,
}

impl TenantKey {
    /// Build a tenant key.
    ///
    /// The module path's major-version suffix (`@v0`) is stripped so a
    /// module keeps its state across major-version bumps.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::Configuration`] when either component is empty.
    pub fn new(module_path: impl AsRef<str>, project: impl Into<String>) -> Result<Self> {
        let module_path = strip_major_version(module_path.as_ref().trim()).to_string();
        let project = project.into();
        if module_path.is_empty() {
            return Err(InfrastructureError::configuration(
                "infrastructure state requires a CUE module path; set `module:` in cue.mod/module.cue",
            ));
        }
        if project.trim().is_empty() {
            return Err(InfrastructureError::configuration(
                "infrastructure state requires a project name; set `name:` on the project",
            ));
        }
        Ok(Self {
            module_path,
            project,
            environment: None,
        })
    }

    /// Build a separate state identity for a named environment.
    ///
    /// # Errors
    ///
    /// Rejects an empty name or one containing control characters.
    pub fn with_environment(
        module_path: impl AsRef<str>,
        project: impl Into<String>,
        environment: impl Into<String>,
    ) -> Result<Self> {
        let mut key = Self::new(module_path, project)?;
        let environment = environment.into();
        if environment.trim().is_empty()
            || environment.trim() != environment
            || environment.chars().any(char::is_control)
        {
            return Err(InfrastructureError::configuration(
                "infrastructure environment must be a nonempty name without surrounding whitespace or control characters",
            ));
        }
        key.environment = Some(environment);
        Ok(key)
    }

    /// CUE module path (tenant).
    #[must_use]
    pub fn module_path(&self) -> &str {
        &self.module_path
    }

    /// Project name (discriminator within the tenant).
    #[must_use]
    pub fn project(&self) -> &str {
        &self.project
    }

    /// Selected named environment, or `None` for the legacy state.
    #[must_use]
    pub fn environment(&self) -> Option<&str> {
        self.environment.as_deref()
    }
}

impl fmt::Display for TenantKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}#{}", self.module_path, self.project)?;
        if let Some(environment) = &self.environment {
            write!(formatter, "@{environment}")?;
        }
        Ok(())
    }
}

/// The CUE instance a project was evaluated from, as
/// `<directory relative to the module root>:<package>`.
///
/// A tenant's state records which instance owns it (see
/// [`crate::state::TenantOwner`]), so a second instance declaring the same
/// project name (in a directory the module loader skips, a nested module
/// with the same module path, or another package) cannot act on it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ProjectInstance(String);

impl ProjectInstance {
    /// Build the identity of the instance in `directory` (relative to the
    /// module root, `/`-separated; empty or `.` for the root) with CUE
    /// package `package`.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::Configuration`] when the package is
    /// empty or contains `:`, or either part contains control characters,
    /// or the directory is absolute or climbs out of the module.
    pub fn new(directory: &str, package: &str) -> Result<Self> {
        let normalized = directory.replace('\\', "/");
        let trimmed = normalized
            .trim_start_matches("./")
            .trim_end_matches('/')
            .to_string();
        let directory = if trimmed.is_empty() {
            ".".to_string()
        } else {
            trimmed
        };
        if package.is_empty() || package.contains(':') {
            return Err(InfrastructureError::configuration(format!(
                "invalid CUE package name '{}' for an instance identity",
                crate::error::strip_control_characters(package)
            )));
        }
        if directory.starts_with('/') || directory.split('/').any(|part| part == "..") {
            return Err(InfrastructureError::configuration(format!(
                "instance directory '{}' must be relative to the module root",
                crate::error::strip_control_characters(&directory)
            )));
        }
        if directory
            .chars()
            .chain(package.chars())
            .any(char::is_control)
        {
            return Err(InfrastructureError::configuration(
                "instance directory and package must not contain control characters",
            ));
        }
        Ok(Self(format!("{directory}:{package}")))
    }

    /// An identity read back from the state store. Control characters are
    /// removed, since the text is displayed.
    #[must_use]
    pub fn from_stored(text: &str) -> Self {
        Self(crate::error::strip_control_characters(text))
    }

    /// The identity as stored: `<directory>:<package>`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProjectInstance {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

fn strip_major_version(path: &str) -> &str {
    path.rsplit_once('@').map_or(path, |(base, _)| base)
}

/// Read the `module:` path from `<module_root>/cue.mod/module.cue`.
///
/// # Errors
///
/// Returns [`InfrastructureError::Configuration`] when the file is missing or declares no
/// module path.
pub fn read_module_path(module_root: &Path) -> Result<String> {
    let file = module_root.join("cue.mod").join("module.cue");
    let contents = std::fs::read_to_string(&file).map_err(|error| {
        InfrastructureError::input_output(format!("read {}", file.display()), error)
    })?;
    parse_module_path(&contents).ok_or_else(|| {
        InfrastructureError::configuration(format!(
            "{} does not declare a `module:` path; infrastructure state is keyed by it",
            file.display()
        ))
    })
}

/// Extract the top-level `module: "..."` string from module.cue contents.
fn parse_module_path(contents: &str) -> Option<String> {
    contents.lines().find_map(|line| {
        let rest = line.trim_start().strip_prefix("module:")?.trim();
        let quoted = rest.strip_prefix('"')?;
        let end = quoted.find('"')?;
        let path = &quoted[..end];
        (!path.is_empty()).then(|| path.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_module_path_from_module_cue() {
        let contents = "module: \"github.com/acme/infrastructure@v0\"\nlanguage: {\n\tversion: \"v0.14.1\"\n}\n";
        assert_eq!(
            parse_module_path(contents).as_deref(),
            Some("github.com/acme/infrastructure@v0")
        );
    }

    #[test]
    fn ignores_nested_or_missing_module_fields() {
        assert_eq!(parse_module_path("language: {version: \"v0.14.1\"}"), None);
        assert_eq!(parse_module_path("module: \"\""), None);
    }

    #[test]
    fn tenant_strips_major_version_suffix() {
        let tenant = TenantKey::new("github.com/acme/infrastructure@v1", "web").unwrap();
        assert_eq!(tenant.module_path(), "github.com/acme/infrastructure");
        assert_eq!(tenant.to_string(), "github.com/acme/infrastructure#web");
    }

    #[test]
    fn tenant_requires_both_components() {
        assert!(TenantKey::new("", "web").is_err());
        assert!(TenantKey::new("github.com/acme/infrastructure", " ").is_err());
    }

    #[test]
    fn named_environment_is_distinct_from_legacy_and_default_is_not_implicit() {
        let legacy = TenantKey::new("example.com/app@v1", "web").unwrap();
        let default = TenantKey::with_environment("example.com/app@v1", "web", "default").unwrap();
        let dev = TenantKey::with_environment("example.com/app@v1", "web", "Dev").unwrap();
        assert_eq!(legacy.environment(), None);
        assert_eq!(legacy.to_string(), "example.com/app#web");
        assert_eq!(default.environment(), Some("default"));
        assert_eq!(default.to_string(), "example.com/app#web@default");
        assert_ne!(legacy, default);
        assert_ne!(default, dev);
        assert!(TenantKey::with_environment("example.com/app", "web", "").is_err());
        assert!(TenantKey::with_environment("example.com/app", "web", " ").is_err());
        assert!(TenantKey::with_environment("example.com/app", "web", "Dev\n").is_err());
    }

    #[test]
    fn project_instances_join_directory_and_package() {
        assert_eq!(
            ProjectInstance::new("", "infrastructure").unwrap().as_str(),
            ".:infrastructure"
        );
        assert_eq!(
            ProjectInstance::new("./services/web/", "web")
                .unwrap()
                .as_str(),
            "services/web:web"
        );
        assert_eq!(
            ProjectInstance::new("_hidden", "web").unwrap().to_string(),
            "_hidden:web"
        );
        assert!(ProjectInstance::new(".", "").is_err());
        assert!(ProjectInstance::new(".", "a:b").is_err());
        assert!(ProjectInstance::new("/absolute", "web").is_err());
        assert!(ProjectInstance::new("../outside", "web").is_err());
        assert!(ProjectInstance::new("a\u{1b}b", "web").is_err());
        assert_eq!(
            ProjectInstance::from_stored("a\u{1b}[2J:web").as_str(),
            "a[2J:web"
        );
    }

    #[test]
    fn reads_module_path_from_disk() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(directory.path().join("cue.mod")).unwrap();
        std::fs::write(
            directory.path().join("cue.mod/module.cue"),
            "module: \"example.com/app\"\n",
        )
        .unwrap();
        assert_eq!(
            read_module_path(directory.path()).unwrap(),
            "example.com/app"
        );
        assert!(read_module_path(&directory.path().join("missing")).is_err());
    }
}
