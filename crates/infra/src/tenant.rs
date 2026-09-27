//! Tenant identity for multi-tenant state.
//!
//! Every state row and lock is scoped by the CUE module path (the tenant)
//! and the cuenv project name (the discriminator within the tenant). There
//! is deliberately no way to address state without both.

use std::fmt;
use std::path::Path;

use crate::error::{InfraError, Result};

/// The owner of a set of managed resources.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TenantKey {
    module_path: String,
    project: String,
}

impl TenantKey {
    /// Build a tenant key.
    ///
    /// The module path's major-version suffix (`@v0`) is stripped so a
    /// module keeps its state across major-version bumps.
    ///
    /// # Errors
    ///
    /// Returns [`InfraError::Config`] when either component is empty.
    pub fn new(module_path: impl AsRef<str>, project: impl Into<String>) -> Result<Self> {
        let module_path = strip_major_version(module_path.as_ref().trim()).to_string();
        let project = project.into();
        if module_path.is_empty() {
            return Err(InfraError::config(
                "infra state requires a CUE module path; set `module:` in cue.mod/module.cue",
            ));
        }
        if project.trim().is_empty() {
            return Err(InfraError::config(
                "infra state requires a project name; set `name:` on the project",
            ));
        }
        Ok(Self {
            module_path,
            project,
        })
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
}

impl fmt::Display for TenantKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#{}", self.module_path, self.project)
    }
}

fn strip_major_version(path: &str) -> &str {
    path.rsplit_once('@').map_or(path, |(base, _)| base)
}

/// Read the `module:` path from `<module_root>/cue.mod/module.cue`.
///
/// # Errors
///
/// Returns [`InfraError::Config`] when the file is missing or declares no
/// module path.
pub fn read_module_path(module_root: &Path) -> Result<String> {
    let file = module_root.join("cue.mod").join("module.cue");
    let contents = std::fs::read_to_string(&file)
        .map_err(|e| InfraError::io(format!("read {}", file.display()), e))?;
    parse_module_path(&contents).ok_or_else(|| {
        InfraError::config(format!(
            "{} does not declare a `module:` path; infra state is keyed by it",
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
        let contents =
            "module: \"github.com/acme/infra@v0\"\nlanguage: {\n\tversion: \"v0.14.1\"\n}\n";
        assert_eq!(
            parse_module_path(contents).as_deref(),
            Some("github.com/acme/infra@v0")
        );
    }

    #[test]
    fn ignores_nested_or_missing_module_fields() {
        assert_eq!(parse_module_path("language: {version: \"v0.14.1\"}"), None);
        assert_eq!(parse_module_path("module: \"\""), None);
    }

    #[test]
    fn tenant_strips_major_version_suffix() {
        let tenant = TenantKey::new("github.com/acme/infra@v1", "web").unwrap();
        assert_eq!(tenant.module_path(), "github.com/acme/infra");
        assert_eq!(tenant.to_string(), "github.com/acme/infra#web");
    }

    #[test]
    fn tenant_requires_both_components() {
        assert!(TenantKey::new("", "web").is_err());
        assert!(TenantKey::new("github.com/acme/infra", " ").is_err());
    }

    #[test]
    fn reads_module_path_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("cue.mod")).unwrap();
        std::fs::write(
            dir.path().join("cue.mod/module.cue"),
            "module: \"example.com/app\"\n",
        )
        .unwrap();
        assert_eq!(read_module_path(dir.path()).unwrap(), "example.com/app");
        assert!(read_module_path(&dir.path().join("missing")).is_err());
    }
}
