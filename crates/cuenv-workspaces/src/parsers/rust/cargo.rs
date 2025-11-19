use crate::core::traits::LockfileParser;
use crate::core::types::{DependencyRef, DependencySource, LockfileEntry};
use crate::error::{Error, Result};
use cargo_lock::{Lockfile, dependency::Dependency as CargoDependency, package::Package};
use cargo_toml::{Error as CargoManifestError, Manifest};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Parser for Cargo `Cargo.lock` files.
///
/// This parser is responsible for reading and interpreting the resolved dependency graph
/// from `Cargo.lock`. It identifies workspace members by cross-referencing the workspace
/// manifest at `Cargo.toml`, but does NOT handle workspace dependency inheritance
/// (e.g., `{ workspace = true }`) - that responsibility belongs to higher layers such as
/// `WorkspaceDiscovery` or `DependencyResolver`.
///
/// The parser outputs `LockfileEntry` instances containing:
/// - Resolved package names and versions from the lockfile
/// - Workspace membership status (determined by checking workspace member list)
/// - Dependency sources (registry, git, path, or workspace)
/// - Checksums and dependency references
///
/// **Note on `{ workspace = true }` inheritance:**
/// This parser works exclusively with the lockfile's resolved dependency graph.
/// Dependency declarations in member `Cargo.toml` files that use `{ workspace = true }`
/// are already resolved to concrete versions in the lockfile. Detecting or processing
/// such inheritance patterns is the responsibility of separate workspace discovery logic.
///
/// **Error handling:**
/// - Returns `Error::LockfileParseFailed` if the lockfile cannot be read or parsed
/// - Returns `Error::ManifestNotFound` if the sibling `Cargo.toml` is missing
/// - The manifest is required to determine workspace membership; without it, parsing fails
#[derive(Debug, Default, Clone, Copy)]
pub struct CargoLockfileParser;

impl LockfileParser for CargoLockfileParser {
    fn parse(&self, lockfile_path: &Path) -> Result<Vec<LockfileEntry>> {
        let lockfile = Lockfile::load(lockfile_path).map_err(|err| Error::LockfileParseFailed {
            path: lockfile_path.to_path_buf(),
            message: err.to_string(),
        })?;

        let workspace_root = lockfile_path
            .parent()
            .ok_or_else(|| Error::LockfileParseFailed {
                path: lockfile_path.to_path_buf(),
                message: "Cargo.lock must reside within a workspace directory".to_string(),
            })?;

        let cargo_toml_path = workspace_root.join("Cargo.toml");
        if !cargo_toml_path.is_file() {
            return Err(Error::ManifestNotFound {
                path: cargo_toml_path,
            });
        }

        let workspace_members = load_workspace_members(&cargo_toml_path)?;
        let packages = lockfile.packages;
        let mut entries = Vec::with_capacity(packages.len());

        for package in &packages {
            let name = package.name.to_string();
            let version = package.version.to_string();
            let is_workspace_member = is_workspace_member(&name, &workspace_members);
            let source = determine_source(package, workspace_root, &workspace_members);
            let checksum = package.checksum.as_ref().map(ToString::to_string);
            let dependencies = map_cargo_dependencies(&package.dependencies);

            entries.push(LockfileEntry {
                name,
                version,
                source,
                checksum,
                dependencies,
                is_workspace_member,
            });
        }

        Ok(entries)
    }

    fn supports_lockfile(&self, path: &Path) -> bool {
        matches!(
            path.file_name().and_then(|n| n.to_str()),
            Some("Cargo.lock")
        )
    }

    fn lockfile_name(&self) -> &'static str {
        "Cargo.lock"
    }
}

type WorkspaceMembers = HashMap<String, PathBuf>;

fn map_manifest_error(path: &Path, err: CargoManifestError) -> Error {
    match err {
        CargoManifestError::Parse(source) => Error::Toml {
            source,
            path: Some(path.to_path_buf()),
        },
        CargoManifestError::Io(source) => Error::Io {
            source,
            path: Some(path.to_path_buf()),
            operation: "reading Cargo manifest".to_string(),
        },
        CargoManifestError::Workspace(inner) => Error::LockfileParseFailed {
            path: path.to_path_buf(),
            message: format!("workspace manifest error: {inner}"),
        },
        CargoManifestError::WorkspaceIntegrity(message) => Error::LockfileParseFailed {
            path: path.to_path_buf(),
            message,
        },
        CargoManifestError::InheritedUnknownValue => Error::LockfileParseFailed {
            path: path.to_path_buf(),
            message: "workspace manifest uses inherited values that have not been resolved"
                .to_string(),
        },
        CargoManifestError::Other(message) => Error::LockfileParseFailed {
            path: path.to_path_buf(),
            message: message.to_string(),
        },
        _ => Error::LockfileParseFailed {
            path: path.to_path_buf(),
            message: "unknown manifest error".to_string(),
        },
    }
}

/// Loads workspace members for the lockfile parser.
///
/// This is a minimal implementation that uses glob expansion to discover workspace members
/// as needed by the lockfile parser. Note that this overlaps with responsibilities that will
/// eventually belong to a dedicated `WorkspaceDiscovery` implementation for Cargo.
///
/// **Future refactoring:**
/// Once a full `CargoWorkspaceDiscovery` is implemented, this logic should either:
/// - Delegate to that discovery implementation, or
/// - Be simplified to avoid duplication of glob/exclude handling
///
/// For now, this provides the necessary member discovery to support lockfile parsing while
/// keeping the parser functional.
fn load_workspace_members(cargo_toml_path: &Path) -> Result<WorkspaceMembers> {
    let mut manifest = Manifest::from_path(cargo_toml_path)
        .map_err(|err| map_manifest_error(cargo_toml_path, err))?;

    let workspace_root = cargo_toml_path
        .parent()
        .ok_or_else(|| Error::LockfileParseFailed {
            path: cargo_toml_path.to_path_buf(),
            message: "Workspace root could not be determined".to_string(),
        })?;

    let mut members = HashMap::new();

    if let Some(workspace) = manifest.workspace.as_ref() {
        let excludes = &workspace.exclude;

        if !workspace.members.is_empty() {
            collect_members(workspace_root, &workspace.members, excludes, &mut members)?;
        }

        if !workspace.default_members.is_empty() {
            collect_members(
                workspace_root,
                &workspace.default_members,
                excludes,
                &mut members,
            )?;
        }
    } else if let Some(package) = manifest.package.take() {
        members.insert(package.name, PathBuf::from("."));
    }

    if members.is_empty() {
        // Accept single-package repositories as valid workspaces (already handled above).
        // This error only occurs if no members were found at all, which implies an invalid
        // or empty workspace definition that doesn't match any members.
        return Err(Error::LockfileParseFailed {
            path: cargo_toml_path.to_path_buf(),
            message: "No workspace members or package declared in Cargo.toml".to_string(),
        });
    }

    Ok(members)
}

/// Collects workspace members by expanding glob patterns.
///
/// This is an internal helper for `load_workspace_members`. It handles both explicit paths
/// and glob patterns (e.g., `crates/*`), respecting exclude patterns.
///
/// **Note:** This logic will eventually be replaced or shared with `WorkspaceDiscovery`.
fn collect_members(
    workspace_root: &Path,
    member_patterns: &[String],
    exclude_patterns: &[String],
    members: &mut WorkspaceMembers,
) -> Result<()> {
    use glob::glob;

    for pattern in member_patterns {
        // Check if the pattern contains glob syntax
        let has_glob = pattern.contains('*') || pattern.contains('?') || pattern.contains('[');

        if has_glob {
            // Expand glob pattern
            let glob_pattern = workspace_root.join(pattern);
            let glob_str = glob_pattern
                .to_str()
                .ok_or_else(|| Error::LockfileParseFailed {
                    path: workspace_root.to_path_buf(),
                    message: format!("Invalid UTF-8 in glob pattern: {pattern}"),
                })?;

            let entries = glob(glob_str).map_err(|err| Error::LockfileParseFailed {
                path: workspace_root.to_path_buf(),
                message: format!("Invalid glob pattern '{pattern}': {err}"),
            })?;

            for entry in entries {
                let member_dir = entry.map_err(|err| Error::LockfileParseFailed {
                    path: workspace_root.to_path_buf(),
                    message: format!("Glob error for pattern '{pattern}': {err}"),
                })?;

                if !member_dir.is_dir() {
                    continue;
                }

                // Check if this member should be excluded
                if should_exclude(workspace_root, &member_dir, exclude_patterns) {
                    continue;
                }

                process_member_dir(workspace_root, &member_dir, members)?;
            }
        } else {
            // Handle explicit path
            let member_dir = workspace_root.join(pattern);

            // Check if this member should be excluded
            if should_exclude(workspace_root, &member_dir, exclude_patterns) {
                continue;
            }

            process_member_dir(workspace_root, &member_dir, members)?;
        }
    }

    Ok(())
}

/// Checks if a member directory should be excluded based on exclude patterns.
///
/// Internal helper that supports both exact matches and glob patterns.
fn should_exclude(workspace_root: &Path, member_dir: &Path, exclude_patterns: &[String]) -> bool {
    use glob::Pattern;

    let Ok(relative_path) = member_dir.strip_prefix(workspace_root) else {
        return false;
    };

    let Some(path_str) = relative_path.to_str() else {
        return false;
    };

    for exclude_pattern in exclude_patterns {
        // Check if the exclude pattern contains glob syntax
        let has_glob = exclude_pattern.contains('*')
            || exclude_pattern.contains('?')
            || exclude_pattern.contains('[');

        if has_glob {
            // Use glob pattern matching
            if let Ok(pattern) = Pattern::new(exclude_pattern)
                && pattern.matches(path_str)
            {
                return true;
            }
        } else {
            // Exact match
            if path_str == exclude_pattern {
                return true;
            }
        }
    }

    false
}

/// Processes a member directory by reading its manifest and adding to the members map.
///
/// Internal helper that reads the member's `Cargo.toml` to extract the package name.
fn process_member_dir(
    workspace_root: &Path,
    member_dir: &Path,
    members: &mut WorkspaceMembers,
) -> Result<()> {
    let manifest_path = member_dir.join("Cargo.toml");

    if !manifest_path.is_file() {
        return Ok(());
    }

    let mut manifest = Manifest::from_path(&manifest_path)
        .map_err(|err| map_manifest_error(&manifest_path, err))?;

    if let Some(package) = manifest.package.take() {
        let name = package.name;
        let relative_path = member_dir
            .strip_prefix(workspace_root)
            .map_or_else(|_| member_dir.to_path_buf(), PathBuf::from);
        members.entry(name).or_insert(relative_path);
    }

    Ok(())
}

fn is_workspace_member(package_name: &str, workspace_members: &WorkspaceMembers) -> bool {
    workspace_members.contains_key(package_name)
}

/// Determines the dependency source from a lockfile package entry.
///
/// This function uses the `SourceId` API to robustly identify the source type instead of
/// relying on string prefix checks. It handles:
/// - Workspace members (identified by name lookup)
/// - Git dependencies (via `SourceId::is_git()`)
/// - Path dependencies (via `SourceId::is_path()`)
/// - Registry dependencies (via `SourceId::is_registry()`)
fn determine_source(
    package: &Package,
    workspace_root: &Path,
    workspace_members: &WorkspaceMembers,
) -> DependencySource {
    let name = package.name.to_string();
    if let Some(relative) = workspace_members.get(&name) {
        return DependencySource::Workspace(relative.clone());
    }

    if let Some(source) = package.source.as_ref() {
        if source.is_git() {
            DependencySource::Git(source.url().to_string())
        } else if source.is_path() {
            let url_str = source.url().to_string();
            DependencySource::Path(extract_path_from_url(&url_str, workspace_root))
        } else if source.is_registry() {
            DependencySource::Registry(source.url().to_string())
        } else {
            // Fallback for other source kinds (e.g., directory sources)
            // Treat them as path dependencies
            let url_str = source.url().to_string();
            DependencySource::Path(extract_path_from_url(&url_str, workspace_root))
        }
    } else {
        // No source means it's from the default registry (crates.io)
        DependencySource::Registry("https://github.com/rust-lang/crates.io-index".to_string())
    }
}

fn map_cargo_dependencies(deps: &[CargoDependency]) -> Vec<DependencyRef> {
    deps.iter()
        .map(|dep| DependencyRef {
            name: dep.name.to_string(),
            version_req: dep.version.to_string(),
        })
        .collect()
}

/// Extracts a filesystem path from a path-based source URL.
///
/// Handles `file://` URLs by stripping the scheme and converting to a filesystem path.
/// Normalizes the path to be relative to the workspace root when possible.
///
/// Note: This is a simplified implementation. For production use, consider using the
/// `url` crate's `Url::to_file_path()` for proper URL decoding and platform-specific handling.
fn extract_path_from_url(url_str: &str, workspace_root: &Path) -> PathBuf {
    // Strip Cargo's `path+file://` prefix before falling back to vanilla `file://`
    let path_str = url_str
        .strip_prefix("path+file://")
        .or_else(|| url_str.strip_prefix("file://"))
        .unwrap_or(url_str);

    // Convert to PathBuf
    let path = PathBuf::from(path_str);

    // Try to make the path relative to the workspace root
    path.strip_prefix(workspace_root)
        .map(PathBuf::from)
        .unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;
    use std::fs;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn test_parses_basic_cargo_lock() {
        let temp = TempDir::new().unwrap();
        let workspace_root = temp.path();
        write_workspace_manifest(workspace_root, &["crates/app"], &[]);
        write_member_manifest(workspace_root.join("crates/app"), "app", "0.1.0");

        let lock_contents = r#"version = 4

[[package]]
name = "app"
version = "0.1.0"
dependencies = [
    "serde 1.0.0",
]

[[package]]
name = "serde"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234"
"#;
        write_lockfile(workspace_root, lock_contents);

        let parser = CargoLockfileParser;
        let entries = parser.parse(&workspace_root.join("Cargo.lock")).unwrap();
        assert_eq!(entries.len(), 2);

        let app = entries.iter().find(|e| e.name == "app").unwrap();
        assert!(app.is_workspace_member);
        assert!(matches!(app.source, DependencySource::Workspace(_)));
        assert_eq!(app.dependencies.len(), 1);

        let serde = entries.iter().find(|e| e.name == "serde").unwrap();
        assert!(!serde.is_workspace_member);
        assert!(matches!(serde.source, DependencySource::Registry(_)));
    }

    #[test]
    fn test_identifies_workspace_members() {
        let temp = TempDir::new().unwrap();
        let workspace_root = temp.path();
        write_workspace_manifest(workspace_root, &["crates/app", "crates/shared"], &[]);
        write_member_manifest(workspace_root.join("crates/app"), "app", "0.1.0");
        write_member_manifest(workspace_root.join("crates/shared"), "shared", "0.1.0");

        let lock_contents = r#"version = 4

[[package]]
name = "app"
version = "0.1.0"
dependencies = [
    "shared 0.1.0",
]

[[package]]
name = "shared"
version = "0.1.0"
"#;
        write_lockfile(workspace_root, lock_contents);

        let parser = CargoLockfileParser;
        let entries = parser.parse(&workspace_root.join("Cargo.lock")).unwrap();
        let app = entries.iter().find(|e| e.name == "app").unwrap();
        let shared = entries.iter().find(|e| e.name == "shared").unwrap();

        assert!(app.is_workspace_member);
        assert!(shared.is_workspace_member);
    }

    #[test]
    fn test_handles_resolved_workspace_dependencies() {
        // This test verifies that the parser correctly handles workspace-internal dependencies
        // as they appear in the lockfile (already resolved to concrete versions).
        // It does NOT test `{ workspace = true }` inheritance from Cargo.toml - that is the
        // responsibility of WorkspaceDiscovery/DependencyResolver layers.
        let temp = TempDir::new().unwrap();
        let workspace_root = temp.path();
        write_workspace_manifest(
            workspace_root,
            &["crates/api", "crates/shared"],
            &["shared"],
        );
        write_member_manifest(workspace_root.join("crates/api"), "api", "0.1.0");
        write_member_manifest(workspace_root.join("crates/shared"), "shared", "0.2.0");

        let lock_contents = r#"version = 4

[[package]]
name = "api"
version = "0.1.0"
dependencies = [
    "shared 0.2.0",
]

[[package]]
name = "shared"
version = "0.2.0"
"#;
        write_lockfile(workspace_root, lock_contents);

        let parser = CargoLockfileParser;
        let entries = parser.parse(&workspace_root.join("Cargo.lock")).unwrap();
        let api = entries.iter().find(|e| e.name == "api").unwrap();
        assert_eq!(api.dependencies[0].name, "shared");
        assert_eq!(api.dependencies[0].version_req, "0.2.0");
    }

    #[test]
    fn test_supports_lockfile() {
        let parser = CargoLockfileParser;
        assert!(parser.supports_lockfile(Path::new("Cargo.lock")));
        assert!(!parser.supports_lockfile(Path::new("package-lock.json")));
    }

    #[test]
    fn test_lockfile_name() {
        let parser = CargoLockfileParser;
        assert_eq!(parser.lockfile_name(), "Cargo.lock");
    }

    fn write_workspace_manifest(root: &Path, members: &[&str], default_members: &[&str]) {
        write_workspace_manifest_with_excludes(root, members, default_members, &[]);
    }

    fn write_workspace_manifest_with_excludes(
        root: &Path,
        members: &[&str],
        default_members: &[&str],
        excludes: &[&str],
    ) {
        fs::create_dir_all(root).unwrap();
        let mut content = String::from("[workspace]\n");
        content.push_str("members = [\n");
        for member in members {
            content.push_str(&format!("    \"{}\",\n", member));
        }
        content.push_str("]\n");

        if !default_members.is_empty() {
            content.push_str("default-members = [\n");
            for member in default_members {
                content.push_str(&format!("    \"{}\",\n", member));
            }
            content.push_str("]\n");
        }

        if !excludes.is_empty() {
            content.push_str("exclude = [\n");
            for exclude in excludes {
                content.push_str(&format!("    \"{}\",\n", exclude));
            }
            content.push_str("]\n");
        }

        fs::write(root.join("Cargo.toml"), content).unwrap();
    }

    fn write_member_manifest(dir: PathBuf, name: &str, version: &str) {
        fs::create_dir_all(&dir).unwrap();
        let manifest = format!(
            "[package]\nname = \"{}\"\nversion = \"{}\"\n",
            name, version
        );
        fs::write(dir.join("Cargo.toml"), manifest).unwrap();
    }

    fn write_lockfile(root: &Path, contents: &str) {
        fs::write(root.join("Cargo.lock"), contents).unwrap();
    }

    #[test]
    fn test_glob_pattern_members() {
        let temp = TempDir::new().unwrap();
        let workspace_root = temp.path();

        // Create workspace with glob pattern
        write_workspace_manifest(workspace_root, &["crates/*"], &[]);

        // Create multiple crates matching the pattern
        write_member_manifest(workspace_root.join("crates/app"), "app", "0.1.0");
        write_member_manifest(workspace_root.join("crates/lib"), "lib", "0.1.0");
        write_member_manifest(workspace_root.join("crates/utils"), "utils", "0.1.0");

        let lock_contents = r#"version = 4

[[package]]
name = "app"
version = "0.1.0"

[[package]]
name = "lib"
version = "0.1.0"

[[package]]
name = "utils"
version = "0.1.0"
"#;
        write_lockfile(workspace_root, lock_contents);

        let parser = CargoLockfileParser;
        let entries = parser.parse(&workspace_root.join("Cargo.lock")).unwrap();

        assert_eq!(entries.len(), 3);

        let app = entries.iter().find(|e| e.name == "app").unwrap();
        let lib = entries.iter().find(|e| e.name == "lib").unwrap();
        let utils = entries.iter().find(|e| e.name == "utils").unwrap();

        assert!(app.is_workspace_member);
        assert!(lib.is_workspace_member);
        assert!(utils.is_workspace_member);
    }

    #[test]
    fn test_glob_pattern_with_excludes() {
        let temp = TempDir::new().unwrap();
        let workspace_root = temp.path();

        // Create workspace with glob pattern and excludes
        write_workspace_manifest_with_excludes(
            workspace_root,
            &["crates/*"],
            &[],
            &["crates/excluded"],
        );

        // Create multiple crates, one of which should be excluded
        write_member_manifest(workspace_root.join("crates/app"), "app", "0.1.0");
        write_member_manifest(workspace_root.join("crates/lib"), "lib", "0.1.0");
        write_member_manifest(workspace_root.join("crates/excluded"), "excluded", "0.1.0");

        let lock_contents = r#"version = 4

[[package]]
name = "app"
version = "0.1.0"

[[package]]
name = "lib"
version = "0.1.0"

[[package]]
name = "excluded"
version = "0.1.0"
"#;
        write_lockfile(workspace_root, lock_contents);

        let parser = CargoLockfileParser;
        let entries = parser.parse(&workspace_root.join("Cargo.lock")).unwrap();

        let app = entries.iter().find(|e| e.name == "app").unwrap();
        let lib = entries.iter().find(|e| e.name == "lib").unwrap();
        let excluded = entries.iter().find(|e| e.name == "excluded").unwrap();

        assert!(app.is_workspace_member);
        assert!(lib.is_workspace_member);
        assert!(
            !excluded.is_workspace_member,
            "excluded crate should not be a workspace member"
        );
    }

    #[test]
    fn test_glob_pattern_exclude_with_wildcard() {
        let temp = TempDir::new().unwrap();
        let workspace_root = temp.path();

        // Create workspace with glob pattern and wildcard exclude
        write_workspace_manifest_with_excludes(
            workspace_root,
            &["crates/*"],
            &[],
            &["crates/test-*"],
        );

        // Create multiple crates, some matching the exclude pattern
        write_member_manifest(workspace_root.join("crates/app"), "app", "0.1.0");
        write_member_manifest(workspace_root.join("crates/lib"), "lib", "0.1.0");
        write_member_manifest(
            workspace_root.join("crates/test-utils"),
            "test-utils",
            "0.1.0",
        );
        write_member_manifest(
            workspace_root.join("crates/test-helpers"),
            "test-helpers",
            "0.1.0",
        );

        let lock_contents = r#"version = 4

[[package]]
name = "app"
version = "0.1.0"

[[package]]
name = "lib"
version = "0.1.0"

[[package]]
name = "test-utils"
version = "0.1.0"

[[package]]
name = "test-helpers"
version = "0.1.0"
"#;
        write_lockfile(workspace_root, lock_contents);

        let parser = CargoLockfileParser;
        let entries = parser.parse(&workspace_root.join("Cargo.lock")).unwrap();

        let app = entries.iter().find(|e| e.name == "app").unwrap();
        let lib = entries.iter().find(|e| e.name == "lib").unwrap();
        let test_utils = entries.iter().find(|e| e.name == "test-utils").unwrap();
        let test_helpers = entries.iter().find(|e| e.name == "test-helpers").unwrap();

        assert!(app.is_workspace_member);
        assert!(lib.is_workspace_member);
        assert!(
            !test_utils.is_workspace_member,
            "test-utils should be excluded"
        );
        assert!(
            !test_helpers.is_workspace_member,
            "test-helpers should be excluded"
        );
    }

    #[test]
    fn test_mixed_explicit_and_glob_patterns() {
        let temp = TempDir::new().unwrap();
        let workspace_root = temp.path();

        // Create workspace with both explicit paths and glob patterns
        write_workspace_manifest(workspace_root, &["core", "crates/*"], &[]);

        write_member_manifest(workspace_root.join("core"), "core", "0.1.0");
        write_member_manifest(workspace_root.join("crates/app"), "app", "0.1.0");
        write_member_manifest(workspace_root.join("crates/lib"), "lib", "0.1.0");

        let lock_contents = r#"version = 4

[[package]]
name = "core"
version = "0.1.0"

[[package]]
name = "app"
version = "0.1.0"

[[package]]
name = "lib"
version = "0.1.0"
"#;
        write_lockfile(workspace_root, lock_contents);

        let parser = CargoLockfileParser;
        let entries = parser.parse(&workspace_root.join("Cargo.lock")).unwrap();

        assert_eq!(entries.len(), 3);

        let core = entries.iter().find(|e| e.name == "core").unwrap();
        let app = entries.iter().find(|e| e.name == "app").unwrap();
        let lib = entries.iter().find(|e| e.name == "lib").unwrap();

        assert!(core.is_workspace_member);
        assert!(app.is_workspace_member);
        assert!(lib.is_workspace_member);
    }

    #[test]
    fn test_nested_glob_patterns() {
        let temp = TempDir::new().unwrap();
        let workspace_root = temp.path();

        // Create workspace with nested glob pattern
        write_workspace_manifest(workspace_root, &["crates/*/*"], &[]);

        write_member_manifest(workspace_root.join("crates/backend/api"), "api", "0.1.0");
        write_member_manifest(workspace_root.join("crates/backend/db"), "db", "0.1.0");
        write_member_manifest(workspace_root.join("crates/frontend/ui"), "ui", "0.1.0");

        let lock_contents = r#"version = 4

[[package]]
name = "api"
version = "0.1.0"

[[package]]
name = "db"
version = "0.1.0"

[[package]]
name = "ui"
version = "0.1.0"
"#;
        write_lockfile(workspace_root, lock_contents);

        let parser = CargoLockfileParser;
        let entries = parser.parse(&workspace_root.join("Cargo.lock")).unwrap();

        assert_eq!(entries.len(), 3);

        let api = entries.iter().find(|e| e.name == "api").unwrap();
        let db = entries.iter().find(|e| e.name == "db").unwrap();
        let ui = entries.iter().find(|e| e.name == "ui").unwrap();

        assert!(api.is_workspace_member);
        assert!(db.is_workspace_member);
        assert!(ui.is_workspace_member);
    }

    #[test]
    fn test_git_dependency_source() {
        let temp = TempDir::new().unwrap();
        let workspace_root = temp.path();
        write_workspace_manifest(workspace_root, &["crates/app"], &[]);
        write_member_manifest(workspace_root.join("crates/app"), "app", "0.1.0");

        let lock_contents = r#"version = 4

[[package]]
name = "app"
version = "0.1.0"
dependencies = [
    "git-dep 0.1.0",
]

[[package]]
name = "git-dep"
version = "0.1.0"
source = "git+https://github.com/example/git-dep?branch=main#abcdef123456"
"#;
        write_lockfile(workspace_root, lock_contents);

        let parser = CargoLockfileParser;
        let entries = parser.parse(&workspace_root.join("Cargo.lock")).unwrap();
        let git_dep = entries.iter().find(|e| e.name == "git-dep").unwrap();

        assert!(matches!(git_dep.source, DependencySource::Git(_)));
        if let DependencySource::Git(url) = &git_dep.source {
            assert!(url.contains("github.com/example/git-dep"));
        }
    }

    #[test]
    fn test_path_dependency_source() {
        let temp = TempDir::new().unwrap();
        let workspace_root = temp.path();
        write_workspace_manifest(workspace_root, &["crates/app"], &[]);
        write_member_manifest(workspace_root.join("crates/app"), "app", "0.1.0");

        // Create a path dependency outside the workspace
        let external_path = temp.path().join("external/path-dep");
        write_member_manifest(external_path.clone(), "path-dep", "0.1.0");

        let lock_contents = format!(
            r#"version = 4

[[package]]
name = "app"
version = "0.1.0"
dependencies = [
    "path-dep 0.1.0",
]

[[package]]
name = "path-dep"
version = "0.1.0"
source = "path+file://{}"
"#,
            external_path.display()
        );
        write_lockfile(workspace_root, &lock_contents);

        let parser = CargoLockfileParser;
        let entries = parser.parse(&workspace_root.join("Cargo.lock")).unwrap();
        let path_dep = entries.iter().find(|e| e.name == "path-dep").unwrap();

        if let DependencySource::Path(relative) = &path_dep.source {
            let expected = PathBuf::from("external").join("path-dep");
            assert_eq!(relative, &expected);
        } else {
            panic!("expected path dependency source to resolve to a path");
        }
    }

    #[test]
    fn test_registry_dependency_source() {
        let temp = TempDir::new().unwrap();
        let workspace_root = temp.path();
        write_workspace_manifest(workspace_root, &["crates/app"], &[]);
        write_member_manifest(workspace_root.join("crates/app"), "app", "0.1.0");

        let lock_contents = r#"version = 4

[[package]]
name = "app"
version = "0.1.0"
dependencies = [
    "serde",
]

[[package]]
name = "serde"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234"
"#;
        write_lockfile(workspace_root, lock_contents);

        let parser = CargoLockfileParser;
        let entries = parser.parse(&workspace_root.join("Cargo.lock")).unwrap();
        let serde = entries.iter().find(|e| e.name == "serde").unwrap();

        assert!(matches!(serde.source, DependencySource::Registry(_)));
        if let DependencySource::Registry(url) = &serde.source {
            assert!(url.contains("crates.io-index"));
        }
    }

    #[test]
    fn test_invalid_workspace_manifest_reports_toml_error() {
        let temp = TempDir::new().unwrap();
        let workspace_root = temp.path();
        fs::write(
            workspace_root.join("Cargo.toml"),
            "[workspace]\nmembers = [\n",
        )
        .unwrap();

        let lock_contents = r#"version = 4

[[package]]
name = "app"
version = "0.1.0"
"#;
        write_lockfile(workspace_root, lock_contents);

        let parser = CargoLockfileParser;
        let err = parser
            .parse(&workspace_root.join("Cargo.lock"))
            .expect_err("expected manifest parse failure");

        match err {
            Error::Toml { path, .. } => {
                assert_eq!(path, Some(workspace_root.join("Cargo.toml")));
            }
            other => panic!("expected Toml error, got {other:?}"),
        }
    }

    #[test]
    fn test_invalid_member_manifest_reports_toml_error() {
        let temp = TempDir::new().unwrap();
        let workspace_root = temp.path();
        write_workspace_manifest(workspace_root, &["crates/app"], &[]);

        let member_dir = workspace_root.join("crates/app");
        fs::create_dir_all(&member_dir).unwrap();
        fs::write(
            member_dir.join("Cargo.toml"),
            "[package]\nname = \"app\"\nversion = [",
        )
        .unwrap();

        let lock_contents = r#"version = 4

[[package]]
name = "app"
version = "0.1.0"
"#;
        write_lockfile(workspace_root, lock_contents);

        let parser = CargoLockfileParser;
        let err = parser
            .parse(&workspace_root.join("Cargo.lock"))
            .expect_err("expected manifest parse failure");

        match err {
            Error::Toml { path, .. } => {
                assert_eq!(path, Some(member_dir.join("Cargo.toml")));
            }
            other => panic!("expected Toml error, got {other:?}"),
        }
    }
}
