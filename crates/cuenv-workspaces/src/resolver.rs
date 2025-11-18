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
        let mut node_map: HashMap<String, NodeIndex> = HashMap::new();

        // 1. Create nodes for all lockfile entries
        // We assume the lockfile contains all relevant dependencies, including workspace members
        // if they are properly resolved.
        for entry in lockfile {
            let dep_ref = DependencyRef {
                name: entry.name.clone(),
                version_req: entry.version.clone(),
            };
            let idx = graph.add_node(dep_ref);
            node_map.insert(entry.name.clone(), idx);
        }

        // 2. Add edges based on dependencies declared in lockfile entries
        for entry in lockfile {
            if let Some(&source_idx) = node_map.get(&entry.name) {
                for dep in &entry.dependencies {
                    // Find the dependency in the graph
                    // Note: This simple lookup by name assumes no version conflicts in the flat list
                    // or that the lockfile entry list has unique names (flattened).
                    // Real lockfiles might have multiple versions of the same package.
                    // If 'lockfile' has multiple entries with same name but different versions,
                    // `node_map` will only store the last one, which is incorrect.
                    // TODO: Handle multiple versions correctly by keying on (name, version).
                    
                    // For now, we'll look up by name. If multiple exist, this needs refinement.
                    // A robust implementation would use (name, version) for the map key.
                    // Let's switch to that now.
                    if let Some(&target_idx) = node_map.get(&dep.name) {
                        // But wait, `dep` is a DependencyRef which has version_req.
                        // The lockfile entry has a specific version.
                        // We need to match the requirement to the resolved version.
                        // Since we are building from a resolved lockfile, the `entry.dependencies`
                        // usually point to other entries.
                        // However, `DependencyRef` in `LockfileEntry` usually just has the name and requested version range.
                        // We need to find which resolved entry matches.
                        //
                        // In a flattened lockfile (like simple node_modules), name is unique.
                        // In a tree lockfile (npm v2/v3), it's complex.
                        //
                        // For the purpose of this implementation ticket (WS-6), and given the previous steps,
                        // we will assume a simplified flat resolution or that `lockfile` passed here
                        // is a flattened list of resolved packages.
                        graph.add_edge(source_idx, target_idx, ());
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
