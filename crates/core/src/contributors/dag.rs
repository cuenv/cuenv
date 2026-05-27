use crate::tasks::TaskNode;
use std::collections::{BTreeMap, HashMap};

/// Build a map of expected task dependencies for DAG verification
#[must_use]
pub fn build_expected_dag(tasks: &HashMap<String, TaskNode>) -> BTreeMap<String, Vec<String>> {
    let mut dag = BTreeMap::new();

    for (name, node) in tasks {
        let deps = collect_deps_from_node(node);
        dag.insert(name.clone(), deps);
    }

    dag
}

fn collect_deps_from_node(node: &TaskNode) -> Vec<String> {
    match node {
        TaskNode::Task(task) => task
            .depends_on
            .iter()
            .map(|d| d.task_name().to_string())
            .collect(),
        TaskNode::Group(group) => group
            .depends_on
            .iter()
            .map(|d| d.task_name().to_string())
            .collect(),
        TaskNode::Sequence(_) => Vec::new(),
    }
}
