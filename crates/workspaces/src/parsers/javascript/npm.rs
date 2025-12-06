use crate::core::traits::LockfileParser;
use crate::core::types::{DependencyRef, DependencySource, LockfileEntry};
use crate::error::{Error, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Parser for npm `package-lock.json` files (lockfileVersion 3).
#[derive(Debug, Default, Clone, Copy)]
pub struct NpmLockfileParser;

impl LockfileParser for NpmLockfileParser {
    fn parse(&self, lockfile_path: &Path) -> Result<Vec<LockfileEntry>> {
        let contents = fs::read_to_string(lockfile_path).map_err(|source| Error::Io {
            source,
            path: Some(lockfile_path.to_path_buf()),
            operation: "reading package-lock.json".to_string(),
        })?;

        let lockfile: PackageLockV3 =
            serde_json::from_str(&contents).map_err(|source| Error::LockfileParseFailed {
                path: lockfile_path.to_path_buf(),
                message: source.to_string(),
            })?;

        if lockfile.lockfile_version != 3 {
            return Err(Error::LockfileParseFailed {
                path: lockfile_path.to_path_buf(),
                message: format!(
                    "Unsupported lockfileVersion {} – only v3 is supported",
                    lockfile.lockfile_version
                ),
            });
        }

        let workspace_name = lockfile.name.unwrap_or_else(|| "workspace".to_string());
        let workspace_version = lockfile.version.unwrap_or_else(|| "0.0.0".to_string());

        let mut entries = Vec::new();
        for (pkg_path, pkg_entry) in lockfile.packages.unwrap_or_default() {
            if let Some(entry) = entry_from_package(
                lockfile_path,
                &pkg_path,
                &pkg_entry,
                &workspace_name,
                &workspace_version,
            )? {
                entries.push(entry);
            }
        }

        Ok(entries)
    }

    fn supports_lockfile(&self, path: &Path) -> bool {
        matches!(
            path.file_name().and_then(|n| n.to_str()),
            Some("package-lock.json")
        )
    }

    fn lockfile_name(&self) -> &'static str {
        "package-lock.json"
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PackageLockV3 {
    #[serde(default)]
    lockfile_version: u32,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    packages: Option<BTreeMap<String, PackageEntry>>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct PackageEntry {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    resolved: Option<String>,
    #[serde(default)]
    integrity: Option<String>,
    #[serde(default)]
    dependencies: BTreeMap<String, String>,
    #[serde(default, rename = "devDependencies")]
    dev_dependencies: BTreeMap<String, String>,
    #[serde(default, rename = "optionalDependencies")]
    optional_dependencies: BTreeMap<String, String>,
}

fn entry_from_package(
    lockfile_path: &Path,
    pkg_path: &str,
    pkg_entry: &PackageEntry,
    workspace_name: &str,
    workspace_version: &str,
) -> Result<Option<LockfileEntry>> {
    let version = if pkg_path.is_empty() {
        pkg_entry
            .version
            .clone()
            .unwrap_or_else(|| workspace_version.to_string())
    } else {
        pkg_entry
            .version
            .clone()
            .ok_or_else(|| Error::LockfileParseFailed {
                path: lockfile_path.to_path_buf(),
                message: format!("Missing version for package entry '{pkg_path}': {pkg_entry:?}",),
            })?
    };

    Ok(Some(build_entry(
        pkg_path,
        pkg_entry,
        workspace_name,
        version,
    )))
}

fn build_entry(
    pkg_path: &str,
    pkg_entry: &PackageEntry,
    workspace_name: &str,
    version: String,
) -> LockfileEntry {
    let name = infer_package_name(pkg_path, pkg_entry, workspace_name);

    // Workspace member detection based on npm lockfile v3 conventions:
    // 1. Empty path "" is always the workspace root
    // 2. Paths without "node_modules" are workspace members (e.g., "packages/app")
    // 3. Paths containing "node_modules" are external dependencies, even if nested
    //    within a workspace member path (e.g., "packages/app/node_modules/react")
    //
    // This heuristic aligns with npm's documented workspace layout where workspace
    // members are listed by their relative path from the root, and external
    // dependencies are always under a node_modules directory.
    //
    // Note: This approach is conservative and based on npm's consistent lockfile
    // structure. A future enhancement could integrate explicit workspace discovery
    // from package.json "workspaces" field for additional validation.
    let is_workspace_member = pkg_path.is_empty()
        || (!pkg_path.starts_with("node_modules") && !pkg_path.contains("/node_modules/"));

    let source = if is_workspace_member {
        DependencySource::Workspace(workspace_source_path(pkg_path))
    } else {
        DependencySource::Registry(
            pkg_entry
                .resolved
                .clone()
                .unwrap_or_else(|| format!("npm:{name}")),
        )
    };

    let checksum = pkg_entry.integrity.clone();
    let mut dependencies = Vec::new();
    dependencies.extend(map_dependencies(&pkg_entry.dependencies));
    dependencies.extend(map_dependencies(&pkg_entry.dev_dependencies));
    dependencies.extend(map_dependencies(&pkg_entry.optional_dependencies));

    LockfileEntry {
        name,
        version,
        source,
        checksum,
        dependencies,
        is_workspace_member,
    }
}

fn workspace_source_path(pkg_path: &str) -> PathBuf {
    if pkg_path.is_empty() {
        PathBuf::from(".")
    } else {
        PathBuf::from(pkg_path)
    }
}

fn infer_package_name(pkg_path: &str, pkg_entry: &PackageEntry, workspace_name: &str) -> String {
    if let Some(name) = &pkg_entry.name {
        return name.clone();
    }

    if pkg_path.is_empty() {
        return workspace_name.to_string();
    }

    let trimmed = pkg_path.trim_start_matches("node_modules/");
    trimmed.rsplit('/').next().unwrap_or(trimmed).to_string()
}

fn map_dependencies(deps: &BTreeMap<String, String>) -> Vec<DependencyRef> {
    deps.iter()
        .map(|(name, version)| DependencyRef {
            name: name.clone(),
            version_req: version.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn parses_basic_package_lock() {
        let json = r#"{
  "name": "acme-app",
  "version": "1.0.0",
  "lockfileVersion": 3,
  "packages": {
	"": {
	  "name": "acme-app",
	  "version": "1.0.0",
	  "dependencies": {
		"left-pad": "^1.3.0"
	  }
	},
	"node_modules/left-pad": {
	  "version": "1.3.0",
	  "resolved": "https://registry.npmjs.org/left-pad/-/left-pad-1.3.0.tgz",
	  "integrity": "sha512-test",
	  "dependencies": {
		"repeat-string": "^1.6.1"
	  }
	}
  }
}"#;

        let mut file = NamedTempFile::new().unwrap();
        file.write_all(json.as_bytes()).unwrap();

        let parser = NpmLockfileParser;
        let entries = parser.parse(file.path()).unwrap();
        assert_eq!(entries.len(), 2);

        let workspace = entries.iter().find(|e| e.is_workspace_member).unwrap();
        assert_eq!(workspace.name, "acme-app");
        assert_eq!(workspace.version, "1.0.0");
        assert_eq!(workspace.dependencies.len(), 1);

        let dep = entries
            .iter()
            .find(|e| e.name == "left-pad")
            .expect("left-pad entry");
        assert_eq!(dep.version, "1.3.0");
        assert_eq!(dep.checksum.as_deref(), Some("sha512-test"));
        assert_eq!(dep.dependencies.len(), 1);
        assert!(!dep.is_workspace_member);
    }

    #[test]
    fn rejects_wrong_version() {
        let json = r#"{"lockfileVersion": 2, "packages": {}}"#;
        let mut file = NamedTempFile::new().unwrap();
        file.write_all(json.as_bytes()).unwrap();

        let parser = NpmLockfileParser;
        let err = parser.parse(file.path()).unwrap_err();
        match err {
            Error::LockfileParseFailed { message, .. } => {
                assert!(message.contains("lockfileVersion"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn treats_non_node_modules_paths_as_workspace_members() {
        // Test that paths without node_modules are treated as workspace members
        // This is the documented npm workspace convention
        let json = r#"{
  "name": "monorepo",
  "version": "1.0.0",
  "lockfileVersion": 3,
  "packages": {
	"": {
	  "name": "monorepo",
	  "version": "1.0.0"
	},
	"apps/web": {
	  "name": "web",
	  "version": "0.1.0"
	},
	"packages/shared": {
	  "name": "shared",
	  "version": "0.2.0"
	},
	"libs/utils": {
	  "name": "utils",
	  "version": "0.3.0"
	}
  }
}"#;

        let mut file = NamedTempFile::new().unwrap();
        file.write_all(json.as_bytes()).unwrap();

        let parser = NpmLockfileParser;
        let entries = parser.parse(file.path()).unwrap();

        // All 4 entries should be workspace members
        assert_eq!(entries.len(), 4);
        for entry in &entries {
            assert!(
                entry.is_workspace_member,
                "Entry '{}' at path should be a workspace member",
                entry.name
            );
        }

        // Verify specific paths
        let web = entries.iter().find(|e| e.name == "web").unwrap();
        assert!(matches!(web.source, DependencySource::Workspace(_)));

        let shared = entries.iter().find(|e| e.name == "shared").unwrap();
        assert!(matches!(shared.source, DependencySource::Workspace(_)));
    }

    #[test]
    fn distinguishes_workspace_from_nested_node_modules() {
        let json = r#"{
  "name": "workspace-root",
  "version": "1.0.0",
  "lockfileVersion": 3,
  "packages": {
	"": {
	  "name": "workspace-root",
	  "version": "1.0.0"
	},
	"packages/app": {
	  "name": "app",
	  "version": "0.1.0",
	  "dependencies": {
		"react": "^18.0.0"
	  }
	},
	"packages/app/node_modules/react": {
	  "version": "18.2.0",
	  "resolved": "https://registry.npmjs.org/react/-/react-18.2.0.tgz",
	  "integrity": "sha512-test"
	},
	"node_modules/left-pad": {
	  "version": "1.3.0",
	  "resolved": "https://registry.npmjs.org/left-pad/-/left-pad-1.3.0.tgz",
	  "integrity": "sha512-test"
	}
  }
}"#;

        let mut file = NamedTempFile::new().unwrap();
        file.write_all(json.as_bytes()).unwrap();

        let parser = NpmLockfileParser;
        let entries = parser.parse(file.path()).unwrap();

        // Should have 4 entries: workspace root, packages/app (workspace), and 2 registry deps
        assert_eq!(entries.len(), 4);

        // Check workspace root
        let root = entries.iter().find(|e| e.name == "workspace-root").unwrap();
        assert!(root.is_workspace_member);
        assert!(matches!(root.source, DependencySource::Workspace(_)));

        // Check workspace member packages/app
        let app = entries.iter().find(|e| e.name == "app").unwrap();
        assert!(app.is_workspace_member);
        assert!(matches!(app.source, DependencySource::Workspace(_)));

        // Check react from packages/app/node_modules - should be registry dep
        let react_entries: Vec<_> = entries.iter().filter(|e| e.name == "react").collect();
        assert_eq!(react_entries.len(), 1);
        let react = react_entries[0];
        assert!(
            !react.is_workspace_member,
            "React in nested node_modules should not be a workspace member"
        );
        assert!(
            matches!(react.source, DependencySource::Registry(_)),
            "React should be a registry dependency"
        );

        // Check left-pad from node_modules - should be registry dep
        let left_pad = entries.iter().find(|e| e.name == "left-pad").unwrap();
        assert!(!left_pad.is_workspace_member);
        assert!(matches!(left_pad.source, DependencySource::Registry(_)));
    }
}
