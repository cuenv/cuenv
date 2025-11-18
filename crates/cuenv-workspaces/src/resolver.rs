//! Dependency resolution implementation for workspaces.
//!
//! This module provides the [`GenericDependencyResolver`] which implements the
//! [`DependencyResolver`] trait. It builds dependency graphs by combining
//! workspace configuration (manifests) with resolved lockfile data.

use crate::core::traits::{DependencyGraph, DependencyResolver};
use crate::core::types::{
    DependencyRef, LockfileEntry, PackageManager, Workspace,
};
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
                    if let Some(&target_idx) = node_map.get(&(dep.name.clone(), dep.version_req.clone())) {
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
                | PackageManager::YarnModern => self.parse_js_deps(&member.manifest_path)?,
                PackageManager::Cargo => self.parse_rust_deps(&member.manifest_path)?,
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

    fn parse_rust_deps(&self, path: &Path) -> Result<Vec<DependencyRef>> {
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
                    let is_workspace = match value {
                        toml::Value::String(_) => false, // Regular version string
                        toml::Value::Table(t) => {
                            t.get("workspace")
                                .and_then(|v| v.as_bool())
                                .unwrap_or(false)
                        }
                        _ => false,
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
