//! Dependency resolution implementation for workspaces.
//!
//! This module provides the [`GenericDependencyResolver`] which implements the
//! [`DependencyResolver`] trait. It builds dependency graphs by combining
//! workspace configuration (manifests) with resolved lockfile data.

use crate::core::traits::{DependencyGraph, DependencyResolver};
use crate::core::types::{DependencyRef, LockfileEntry, PackageManager, Workspace};
use crate::discovery::read_json_file;
use crate::error::Result;
use petgraph::graph::NodeIndex;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[cfg(feature = "toml")]
use crate::discovery::read_toml_file;

/// A generic dependency resolver that works across supported package managers.
///
/// This resolver implements a strategy-based approach to handle differences between
/// package managers (e.g., npm vs. Cargo) while providing a unified interface
/// for dependency resolution.
pub struct GenericDependencyResolver;

impl DependencyResolver for GenericDependencyResolver {
    fn resolve_dependencies(
        &self,
        _workspace: &Workspace,
        lockfile: &[LockfileEntry],
    ) -> Result<DependencyGraph> {
        let mut graph = DependencyGraph::new();
        // Key by (name, version) to handle multiple versions of the same package
        let mut node_map: HashMap<(String, String), NodeIndex> = HashMap::new();

        // 1. Create nodes for all lockfile entries
        for entry in lockfile {
            let dep_ref = DependencyRef {
                name: entry.name.clone(),
                version_req: entry.version.clone(),
            };
            let idx = graph.add_node(dep_ref);
            node_map.insert((entry.name.clone(), entry.version.clone()), idx);
        }

        // 2. Add edges based on dependencies declared in lockfile entries
        for entry in lockfile {
            if let Some(&source_idx) = node_map.get(&(entry.name.clone(), entry.version.clone())) {
                for dep in &entry.dependencies {
                    // Find the dependency in the graph
                    // We look up by (name, version_req). This assumes that the lockfile parser
                    // populates `version_req` with the *resolved* version for that dependency,
                    // or that the version requirement exactly matches one of the present versions.
                    //
                    // For many lockfiles (like Cargo.lock), the dependency list contains the
                    // concrete version that was resolved.
                    if let Some(&target_idx) =
                        node_map.get(&(dep.name.clone(), dep.version_req.clone()))
                    {
                        graph.add_edge(source_idx, target_idx, ());
                    } else {
                        // Fallback: If strict lookup fails (e.g. version_req is a range like "^1.0.0"),
                        // we would ideally perform semver resolution against available nodes.
                        // For now, we log a trace if we can't find the exact match.
                        // This keeps the graph correct for well-formed resolved lockfiles.
                        tracing::trace!(
                            "Could not find exact match for dependency {} {} -> {} {}",
                            entry.name,
                            entry.version,
                            dep.name,
                            dep.version_req
                        );
                    }
                }
            }
        }

        Ok(graph)
    }

    fn resolve_workspace_deps(&self, workspace: &Workspace) -> Result<Vec<DependencyRef>> {
        let mut workspace_deps = Vec::new();

        for member in &workspace.members {
            let deps = match workspace.manager {
                PackageManager::Npm
                | PackageManager::Bun
                | PackageManager::Pnpm
                | PackageManager::YarnClassic
                | PackageManager::YarnModern
                | PackageManager::Deno => self.parse_js_deps(&member.manifest_path)?,
                PackageManager::Cargo => Self::parse_rust_deps(&member.manifest_path)?,
            };

            workspace_deps.extend(deps);
        }

        Ok(workspace_deps)
    }

    fn resolve_external_deps(&self, lockfile: &[LockfileEntry]) -> Result<Vec<DependencyRef>> {
        Ok(lockfile
            .iter()
            .filter(|entry| !entry.is_workspace_member)
            .map(|entry| DependencyRef {
                name: entry.name.clone(),
                version_req: entry.version.clone(),
            })
            .collect())
    }

    fn detect_workspace_protocol(&self, spec: &str) -> bool {
        // JS: "workspace:*" or "workspace:^1.2.3"
        // Rust: we map { workspace = true } to "workspace" version requirement (internal convention)
        spec.starts_with("workspace:") || spec == "workspace"
    }
}

impl GenericDependencyResolver {
    fn parse_js_deps(&self, path: &Path) -> Result<Vec<DependencyRef>> {
        #[derive(Deserialize)]
        struct PackageJsonDeps {
            dependencies: Option<HashMap<String, String>>,
            #[serde(rename = "devDependencies")]
            dev_dependencies: Option<HashMap<String, String>>,
        }

        let pkg: PackageJsonDeps = read_json_file(path)?;
        let mut result = Vec::new();

        let mut add_deps = |deps: HashMap<String, String>| {
            for (name, version) in deps {
                if self.detect_workspace_protocol(&version) {
                    result.push(DependencyRef {
                        name,
                        version_req: version,
                    });
                }
            }
        };

        if let Some(deps) = pkg.dependencies {
            add_deps(deps);
        }
        if let Some(deps) = pkg.dev_dependencies {
            add_deps(deps);
        }

        Ok(result)
    }

    fn parse_rust_deps(path: &Path) -> Result<Vec<DependencyRef>> {
        #[cfg(feature = "toml")]
        {
            #[derive(Deserialize)]
            struct CargoTomlDeps {
                dependencies: Option<HashMap<String, toml::Value>>,
                #[serde(rename = "dev-dependencies")]
                dev_dependencies: Option<HashMap<String, toml::Value>>,
            }

            // If toml is not available, we can't parse. But if manager is Cargo, toml should be available.
            // We'll return empty if toml is missing but this code block is cfg-gated.
            let pkg: CargoTomlDeps = read_toml_file(path)?;
            let mut result = Vec::new();

            let mut add_deps = |deps: HashMap<String, toml::Value>| {
                for (name, value) in deps {
                    let is_workspace = if let toml::Value::Table(t) = &value {
                        t.get("workspace")
                            .and_then(toml::Value::as_bool)
                            .unwrap_or(false)
                    } else {
                        false
                    };

                    if is_workspace {
                        result.push(DependencyRef {
                            name,
                            version_req: "workspace".to_string(),
                        });
                    }
                }
            };

            if let Some(deps) = pkg.dependencies {
                add_deps(deps);
            }
            if let Some(deps) = pkg.dev_dependencies {
                add_deps(deps);
            }

            Ok(result)
        }
        #[cfg(not(feature = "toml"))]
        {
            // Should not happen if features are configured correctly for Cargo
            let _ = path;
            Ok(Vec::new())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DependencySource;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// Helper to create a `LockfileEntry` for tests
    fn make_entry(name: &str, version: &str, is_workspace: bool) -> LockfileEntry {
        LockfileEntry {
            name: name.to_string(),
            version: version.to_string(),
            source: if is_workspace {
                DependencySource::Workspace(PathBuf::from(name))
            } else {
                DependencySource::Registry("https://registry.npmjs.org".to_string())
            },
            checksum: None,
            dependencies: vec![],
            is_workspace_member: is_workspace,
        }
    }

    /// Helper to create a `LockfileEntry` with dependencies
    fn make_entry_with_deps(name: &str, version: &str, deps: Vec<DependencyRef>) -> LockfileEntry {
        LockfileEntry {
            name: name.to_string(),
            version: version.to_string(),
            source: DependencySource::Registry("https://registry.npmjs.org".to_string()),
            checksum: None,
            dependencies: deps,
            is_workspace_member: false,
        }
    }

    // ==========================================================================
    // GenericDependencyResolver::detect_workspace_protocol tests
    // ==========================================================================

    #[test]
    fn test_detect_workspace_protocol_js_workspace_star() {
        let resolver = GenericDependencyResolver;
        assert!(resolver.detect_workspace_protocol("workspace:*"));
    }

    #[test]
    fn test_detect_workspace_protocol_js_workspace_version() {
        let resolver = GenericDependencyResolver;
        assert!(resolver.detect_workspace_protocol("workspace:^1.2.3"));
        assert!(resolver.detect_workspace_protocol("workspace:~1.0.0"));
    }

    #[test]
    fn test_detect_workspace_protocol_rust_workspace() {
        let resolver = GenericDependencyResolver;
        assert!(resolver.detect_workspace_protocol("workspace"));
    }

    #[test]
    fn test_detect_workspace_protocol_not_workspace() {
        let resolver = GenericDependencyResolver;
        assert!(!resolver.detect_workspace_protocol("^1.0.0"));
        assert!(!resolver.detect_workspace_protocol("1.2.3"));
        assert!(!resolver.detect_workspace_protocol("latest"));
        assert!(!resolver.detect_workspace_protocol(""));
    }

    // ==========================================================================
    // GenericDependencyResolver::resolve_external_deps tests
    // ==========================================================================

    #[test]
    fn test_resolve_external_deps_filters_workspace_members() {
        let resolver = GenericDependencyResolver;
        let lockfile = vec![
            make_entry("external-pkg", "1.0.0", false),
            make_entry("workspace-pkg", "0.1.0", true),
        ];

        let result = resolver.resolve_external_deps(&lockfile).unwrap();

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "external-pkg");
    }

    #[test]
    fn test_resolve_external_deps_empty_lockfile() {
        let resolver = GenericDependencyResolver;
        let lockfile: Vec<LockfileEntry> = vec![];

        let result = resolver.resolve_external_deps(&lockfile).unwrap();

        assert!(result.is_empty());
    }

    #[test]
    fn test_resolve_external_deps_all_workspace_members() {
        let resolver = GenericDependencyResolver;
        let lockfile = vec![
            make_entry("pkg-a", "0.1.0", true),
            make_entry("pkg-b", "0.2.0", true),
        ];

        let result = resolver.resolve_external_deps(&lockfile).unwrap();

        assert!(result.is_empty());
    }

    // ==========================================================================
    // GenericDependencyResolver::resolve_dependencies tests
    // ==========================================================================

    #[test]
    fn test_resolve_dependencies_creates_graph() {
        let resolver = GenericDependencyResolver;
        let workspace = Workspace::new(PathBuf::from("/project"), PackageManager::Npm);
        let lockfile = vec![
            make_entry("pkg-a", "1.0.0", false),
            make_entry("pkg-b", "2.0.0", false),
        ];

        let graph = resolver
            .resolve_dependencies(&workspace, &lockfile)
            .unwrap();

        assert_eq!(graph.node_count(), 2);
    }

    #[test]
    fn test_resolve_dependencies_with_edges() {
        let resolver = GenericDependencyResolver;
        let workspace = Workspace::new(PathBuf::from("/project"), PackageManager::Npm);
        let lockfile = vec![
            make_entry_with_deps(
                "pkg-a",
                "1.0.0",
                vec![DependencyRef {
                    name: "pkg-b".to_string(),
                    version_req: "2.0.0".to_string(),
                }],
            ),
            make_entry("pkg-b", "2.0.0", false),
        ];

        let graph = resolver
            .resolve_dependencies(&workspace, &lockfile)
            .unwrap();

        assert_eq!(graph.node_count(), 2);
        assert_eq!(graph.edge_count(), 1);
    }

    #[test]
    fn test_resolve_dependencies_empty_lockfile() {
        let resolver = GenericDependencyResolver;
        let workspace = Workspace::new(PathBuf::from("/project"), PackageManager::Npm);
        let lockfile: Vec<LockfileEntry> = vec![];

        let graph = resolver
            .resolve_dependencies(&workspace, &lockfile)
            .unwrap();

        assert_eq!(graph.node_count(), 0);
        assert_eq!(graph.edge_count(), 0);
    }

    #[test]
    fn test_resolve_dependencies_missing_dep_skipped() {
        // When a dependency references a package not in the lockfile, it should be skipped
        let resolver = GenericDependencyResolver;
        let workspace = Workspace::new(PathBuf::from("/project"), PackageManager::Npm);
        let lockfile = vec![make_entry_with_deps(
            "pkg-a",
            "1.0.0",
            vec![DependencyRef {
                name: "missing-pkg".to_string(),
                version_req: "1.0.0".to_string(),
            }],
        )];

        let graph = resolver
            .resolve_dependencies(&workspace, &lockfile)
            .unwrap();

        // Node for pkg-a, but no edge to missing-pkg
        assert_eq!(graph.node_count(), 1);
        assert_eq!(graph.edge_count(), 0);
    }

    #[test]
    fn test_resolve_dependencies_multiple_versions() {
        // Multiple versions of the same package should be separate nodes
        let resolver = GenericDependencyResolver;
        let workspace = Workspace::new(PathBuf::from("/project"), PackageManager::Npm);
        let lockfile = vec![
            make_entry("lodash", "4.0.0", false),
            make_entry("lodash", "3.0.0", false),
        ];

        let graph = resolver
            .resolve_dependencies(&workspace, &lockfile)
            .unwrap();

        // Two separate nodes for different versions
        assert_eq!(graph.node_count(), 2);
    }

    // ==========================================================================
    // GenericDependencyResolver::parse_js_deps tests
    // ==========================================================================

    #[test]
    fn test_parse_js_deps_workspace_dependencies() {
        let temp_dir = TempDir::new().unwrap();
        let pkg_json = temp_dir.path().join("package.json");
        fs::write(
            &pkg_json,
            r#"{
            "dependencies": {
                "external": "^1.0.0",
                "workspace-pkg": "workspace:*"
            },
            "devDependencies": {
                "dev-workspace": "workspace:^1.0.0"
            }
        }"#,
        )
        .unwrap();

        let resolver = GenericDependencyResolver;
        let deps = resolver.parse_js_deps(&pkg_json).unwrap();

        // Only workspace deps should be returned
        assert_eq!(deps.len(), 2);
        let names: Vec<&str> = deps.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"workspace-pkg"));
        assert!(names.contains(&"dev-workspace"));
    }

    #[test]
    fn test_parse_js_deps_no_workspace_deps() {
        let temp_dir = TempDir::new().unwrap();
        let pkg_json = temp_dir.path().join("package.json");
        fs::write(
            &pkg_json,
            r#"{
            "dependencies": {
                "lodash": "^4.0.0",
                "react": "^18.0.0"
            }
        }"#,
        )
        .unwrap();

        let resolver = GenericDependencyResolver;
        let deps = resolver.parse_js_deps(&pkg_json).unwrap();

        assert!(deps.is_empty());
    }

    #[test]
    fn test_parse_js_deps_empty_deps() {
        let temp_dir = TempDir::new().unwrap();
        let pkg_json = temp_dir.path().join("package.json");
        fs::write(&pkg_json, r"{}").unwrap();

        let resolver = GenericDependencyResolver;
        let deps = resolver.parse_js_deps(&pkg_json).unwrap();

        assert!(deps.is_empty());
    }

    // ==========================================================================
    // GenericDependencyResolver::parse_rust_deps tests
    // ==========================================================================

    #[cfg(feature = "toml")]
    #[test]
    fn test_parse_rust_deps_workspace_dependencies() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"
[dependencies]
serde = "1.0"
my-lib = { workspace = true }

[dev-dependencies]
test-helper = { workspace = true }
"#,
        )
        .unwrap();

        let deps = GenericDependencyResolver::parse_rust_deps(&cargo_toml).unwrap();

        assert_eq!(deps.len(), 2);
        let names: Vec<&str> = deps.iter().map(|d| d.name.as_str()).collect();
        assert!(names.contains(&"my-lib"));
        assert!(names.contains(&"test-helper"));
    }

    #[cfg(feature = "toml")]
    #[test]
    fn test_parse_rust_deps_no_workspace_deps() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"
[dependencies]
serde = "1.0"
tokio = { version = "1.0", features = ["full"] }
"#,
        )
        .unwrap();

        let deps = GenericDependencyResolver::parse_rust_deps(&cargo_toml).unwrap();

        assert!(deps.is_empty());
    }

    #[cfg(feature = "toml")]
    #[test]
    fn test_parse_rust_deps_workspace_false_ignored() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"
[dependencies]
my-lib = { workspace = false, version = "1.0" }
"#,
        )
        .unwrap();

        let deps = GenericDependencyResolver::parse_rust_deps(&cargo_toml).unwrap();

        assert!(deps.is_empty());
    }

    #[cfg(feature = "toml")]
    #[test]
    fn test_parse_rust_deps_empty() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"
[package]
name = "my-crate"
version = "0.1.0"
"#,
        )
        .unwrap();

        let deps = GenericDependencyResolver::parse_rust_deps(&cargo_toml).unwrap();

        assert!(deps.is_empty());
    }

    #[cfg(feature = "toml")]
    #[test]
    fn test_parse_rust_deps_string_version_ignored() {
        let temp_dir = TempDir::new().unwrap();
        let cargo_toml = temp_dir.path().join("Cargo.toml");
        fs::write(
            &cargo_toml,
            r#"
[dependencies]
serde = "1.0"
"#,
        )
        .unwrap();

        // String version deps should not be treated as workspace deps
        let deps = GenericDependencyResolver::parse_rust_deps(&cargo_toml).unwrap();

        assert!(deps.is_empty());
    }
}
