//! Problems that belong to one node of a project.
//!
//! Reading a project's evaluated value never fails because one task, group,
//! service, image or pipeline is wrong: the problem is recorded against that
//! node and raised only where a task graph, affected detection or CI
//! compilation reaches it. A broken reference in one task then fails the
//! runs that need that task, and no other.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The problems found in a project's nodes, by node.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeDiagnostics {
    /// By task, group or sequence name, as a task graph names it:
    /// `build`, `docs.deploy`, `release[1]`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub tasks: BTreeMap<String, Vec<String>>,
    /// By service name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub services: BTreeMap<String, Vec<String>>,
    /// By image name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub images: BTreeMap<String, Vec<String>>,
    /// By CI pipeline name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub pipelines: BTreeMap<String, Vec<String>>,
}

impl NodeDiagnostics {
    /// Whether no node has a problem.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
            && self.services.is_empty()
            && self.images.is_empty()
            && self.pipelines.is_empty()
    }

    /// The problems of the task, group or sequence a graph calls `name`.
    #[must_use]
    pub fn task(&self, name: &str) -> &[String] {
        self.tasks.get(name).map_or(&[], Vec::as_slice)
    }

    /// The problems of the pipeline called `name`.
    #[must_use]
    pub fn pipeline(&self, name: &str) -> &[String] {
        self.pipelines.get(name).map_or(&[], Vec::as_slice)
    }

    /// The problems of the service called `name`.
    #[must_use]
    pub fn service(&self, name: &str) -> &[String] {
        self.services.get(name).map_or(&[], Vec::as_slice)
    }

    /// The problems of the image called `name`.
    #[must_use]
    pub fn image(&self, name: &str) -> &[String] {
        self.images.get(name).map_or(&[], Vec::as_slice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_node_without_problems_has_none() {
        let mut diagnostics = NodeDiagnostics::default();
        assert!(diagnostics.is_empty());
        diagnostics
            .tasks
            .insert("build".to_string(), vec!["broken".to_string()]);

        assert!(!diagnostics.is_empty());
        assert_eq!(diagnostics.task("build"), ["broken"]);
        assert!(diagnostics.task("test").is_empty());
        assert!(diagnostics.pipeline("default").is_empty());
    }
}
