//! Publishing workflow and topological ordering.
//!
//! This module provides utilities for publishing packages in the correct
//! dependency order, ensuring that dependencies are published before
//! the packages that depend on them.

use crate::error::{Error, Result};
use crate::version::Version;
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::Topo;
use std::collections::HashMap;
use std::path::PathBuf;

/// Represents a package to be published.
#[derive(Debug, Clone)]
pub struct PublishPackage {
    /// Package name.
    pub name: String,
    /// Path to the package root.
    pub path: PathBuf,
    /// New version to publish.
    pub version: Version,
    /// Names of packages this depends on.
    pub dependencies: Vec<String>,
}

/// A plan for publishing packages in the correct order.
#[derive(Debug, Clone)]
pub struct PublishPlan {
    /// Packages in topological order (dependencies first).
    pub packages: Vec<PublishPackage>,
}

impl PublishPlan {
    /// Create a publish plan from a list of packages.
    ///
    /// # Errors
    ///
    /// Returns an error if there is a dependency cycle or missing dependency.
    pub fn from_packages(packages: Vec<PublishPackage>) -> Result<Self> {
        if packages.is_empty() {
            return Ok(Self { packages: vec![] });
        }

        // Build a dependency graph
        let mut graph: DiGraph<String, ()> = DiGraph::new();
        let mut node_map: HashMap<String, NodeIndex> = HashMap::new();

        // Add all nodes
        for pkg in &packages {
            let idx = graph.add_node(pkg.name.clone());
            node_map.insert(pkg.name.clone(), idx);
        }

        // Add edges (dependency -> dependent)
        for pkg in &packages {
            let dependent_idx = node_map[&pkg.name];
            for dep in &pkg.dependencies {
                // Only add edge if the dependency is also being published
                if let Some(&dep_idx) = node_map.get(dep) {
                    graph.add_edge(dep_idx, dependent_idx, ());
                }
            }
        }

        // Topological sort
        let mut topo = Topo::new(&graph);
        let mut ordered_names = Vec::new();

        while let Some(idx) = topo.next(&graph) {
            ordered_names.push(graph[idx].clone());
        }

        // Check for cycles (topo sort would have fewer nodes)
        if ordered_names.len() != packages.len() {
            return Err(Error::config(
                "Dependency cycle detected in packages to publish",
                "Check package dependencies for circular references",
            ));
        }

        // Build ordered package list
        let pkg_map: HashMap<String, PublishPackage> =
            packages.into_iter().map(|p| (p.name.clone(), p)).collect();

        let ordered_packages: Vec<PublishPackage> = ordered_names
            .into_iter()
            .filter_map(|name| pkg_map.get(&name).cloned())
            .collect();

        Ok(Self {
            packages: ordered_packages,
        })
    }

    /// Get the number of packages to publish.
    #[must_use]
    pub fn len(&self) -> usize {
        self.packages.len()
    }

    /// Check if the plan is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.packages.is_empty()
    }

    /// Iterate over packages in publish order.
    pub fn iter(&self) -> impl Iterator<Item = &PublishPackage> {
        self.packages.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_package(name: &str, deps: Vec<&str>) -> PublishPackage {
        PublishPackage {
            name: name.to_string(),
            path: PathBuf::from(format!("packages/{name}")),
            version: Version::new(1, 0, 0),
            dependencies: deps.into_iter().map(String::from).collect(),
        }
    }

    #[test]
    fn test_publish_plan_empty() {
        let plan = PublishPlan::from_packages(vec![]).unwrap();
        assert!(plan.is_empty());
        assert_eq!(plan.len(), 0);
    }

    #[test]
    fn test_publish_plan_single_package() {
        let packages = vec![make_package("pkg-a", vec![])];
        let plan = PublishPlan::from_packages(packages).unwrap();

        assert_eq!(plan.len(), 1);
        assert_eq!(plan.packages[0].name, "pkg-a");
    }

    #[test]
    fn test_publish_plan_linear_deps() {
        // pkg-c depends on pkg-b depends on pkg-a
        let packages = vec![
            make_package("pkg-c", vec!["pkg-b"]),
            make_package("pkg-b", vec!["pkg-a"]),
            make_package("pkg-a", vec![]),
        ];

        let plan = PublishPlan::from_packages(packages).unwrap();

        assert_eq!(plan.len(), 3);
        // pkg-a should come first, then pkg-b, then pkg-c
        assert_eq!(plan.packages[0].name, "pkg-a");
        assert_eq!(plan.packages[1].name, "pkg-b");
        assert_eq!(plan.packages[2].name, "pkg-c");
    }

    #[test]
    fn test_publish_plan_diamond_deps() {
        // pkg-d depends on pkg-b and pkg-c
        // pkg-b depends on pkg-a
        // pkg-c depends on pkg-a
        let packages = vec![
            make_package("pkg-d", vec!["pkg-b", "pkg-c"]),
            make_package("pkg-b", vec!["pkg-a"]),
            make_package("pkg-c", vec!["pkg-a"]),
            make_package("pkg-a", vec![]),
        ];

        let plan = PublishPlan::from_packages(packages).unwrap();

        assert_eq!(plan.len(), 4);
        // pkg-a must come first
        assert_eq!(plan.packages[0].name, "pkg-a");
        // pkg-d must come last
        assert_eq!(plan.packages[3].name, "pkg-d");
        // pkg-b and pkg-c can be in any order in the middle
        let middle: Vec<&str> = plan.packages[1..3]
            .iter()
            .map(|p| p.name.as_str())
            .collect();
        assert!(middle.contains(&"pkg-b"));
        assert!(middle.contains(&"pkg-c"));
    }

    #[test]
    fn test_publish_plan_external_deps() {
        // Dependencies on packages not in the publish list are ignored
        let packages = vec![
            make_package("pkg-a", vec!["external-dep"]),
            make_package("pkg-b", vec!["pkg-a"]),
        ];

        let plan = PublishPlan::from_packages(packages).unwrap();

        assert_eq!(plan.len(), 2);
        assert_eq!(plan.packages[0].name, "pkg-a");
        assert_eq!(plan.packages[1].name, "pkg-b");
    }

    #[test]
    fn test_publish_plan_iteration() {
        let packages = vec![
            make_package("pkg-b", vec!["pkg-a"]),
            make_package("pkg-a", vec![]),
        ];

        let plan = PublishPlan::from_packages(packages).unwrap();

        let names: Vec<&str> = plan.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["pkg-a", "pkg-b"]);
    }
}
