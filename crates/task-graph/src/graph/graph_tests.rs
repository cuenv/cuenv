use super::*;

/// Simple test task implementation
#[derive(Clone, Debug, Default)]
struct TestTask {
    depends_on: Vec<String>,
}

impl TestTask {
    fn new(deps: &[&str]) -> Self {
        Self {
            depends_on: deps.iter().map(|s| (*s).to_string()).collect(),
        }
    }
}

impl TaskNodeData for TestTask {
    fn dependency_names(&self) -> impl Iterator<Item = &str> {
        self.depends_on.iter().map(String::as_str)
    }

    fn add_dependency(&mut self, dep: String) {
        if !self.depends_on.contains(&dep) {
            self.depends_on.push(dep);
        }
    }
}

#[test]
fn test_task_graph_new() {
    let graph: TaskGraph<TestTask> = TaskGraph::new();
    assert_eq!(graph.task_count(), 0);
}

#[test]
fn test_add_single_task() {
    let mut graph = TaskGraph::new();
    let task = TestTask::new(&[]);

    let node = graph.add_task("test", task).unwrap();
    assert!(graph.contains_task("test"));
    assert_eq!(graph.task_count(), 1);

    // Adding same task again should return same node
    let task2 = TestTask::new(&[]);
    let node2 = graph.add_task("test", task2).unwrap();
    assert_eq!(node, node2);
    assert_eq!(graph.task_count(), 1);
}

#[test]
fn test_task_dependencies() {
    let mut graph = TaskGraph::new();

    // Add tasks with dependencies
    let task1 = TestTask::new(&[]);
    let task2 = TestTask::new(&["task1"]);
    let task3 = TestTask::new(&["task1", "task2"]);

    graph.add_task("task1", task1).unwrap();
    graph.add_task("task2", task2).unwrap();
    graph.add_task("task3", task3).unwrap();
    graph.add_dependency_edges().unwrap();

    assert_eq!(graph.task_count(), 3);
    assert!(!graph.has_cycles());

    let sorted = graph.topological_sort().unwrap();
    assert_eq!(sorted.len(), 3);

    // task1 should come before task2 and task3
    let positions: HashMap<String, usize> = sorted
        .iter()
        .enumerate()
        .map(|(i, node)| (node.name.clone(), i))
        .collect();

    assert!(positions["task1"] < positions["task2"]);
    assert!(positions["task1"] < positions["task3"]);
    assert!(positions["task2"] < positions["task3"]);
}

#[test]
fn test_cycle_detection() {
    let mut graph = TaskGraph::new();

    // Create a cycle: task1 -> task2 -> task3 -> task1
    let task1 = TestTask::new(&["task3"]);
    let task2 = TestTask::new(&["task1"]);
    let task3 = TestTask::new(&["task2"]);

    graph.add_task("task1", task1).unwrap();
    graph.add_task("task2", task2).unwrap();
    graph.add_task("task3", task3).unwrap();
    graph.add_dependency_edges().unwrap();

    assert!(graph.has_cycles());
    assert!(graph.topological_sort().is_err());
}

#[test]
fn test_parallel_groups() {
    let mut graph = TaskGraph::new();

    // Create tasks that can run in parallel
    // Level 0: task1, task2 (no dependencies)
    // Level 1: task3 (depends on task1), task4 (depends on task2)
    // Level 2: task5 (depends on task3 and task4)

    let task1 = TestTask::new(&[]);
    let task2 = TestTask::new(&[]);
    let task3 = TestTask::new(&["task1"]);
    let task4 = TestTask::new(&["task2"]);
    let task5 = TestTask::new(&["task3", "task4"]);

    graph.add_task("task1", task1).unwrap();
    graph.add_task("task2", task2).unwrap();
    graph.add_task("task3", task3).unwrap();
    graph.add_task("task4", task4).unwrap();
    graph.add_task("task5", task5).unwrap();
    graph.add_dependency_edges().unwrap();

    let groups = graph.get_parallel_groups().unwrap();

    // Should have 3 levels
    assert_eq!(groups.len(), 3);

    // Level 0 should have 2 tasks
    assert_eq!(groups[0].len(), 2);

    // Level 1 should have 2 tasks
    assert_eq!(groups[1].len(), 2);

    // Level 2 should have 1 task
    assert_eq!(groups[2].len(), 1);
    assert_eq!(groups[2][0].name, "task5");
}

#[test]
fn test_group_dependency_expansion() {
    let mut graph = TaskGraph::new();

    // Register a group "build" with two children
    graph.register_group(
        "build",
        vec!["build.deps".to_string(), "build.compile".to_string()],
    );

    // Add the child tasks
    let deps_task = TestTask::new(&[]);
    let compile_task = TestTask::new(&[]);
    graph.add_task("build.deps", deps_task).unwrap();
    graph.add_task("build.compile", compile_task).unwrap();

    // Add a task that depends on the group name "build"
    let test_task = TestTask::new(&["build"]);
    graph.add_task("test", test_task).unwrap();

    // This should succeed - "build" expands to both children
    graph.add_dependency_edges().unwrap();

    assert!(!graph.has_cycles());
    assert_eq!(graph.task_count(), 3);

    // test should come after both build.deps and build.compile
    let sorted = graph.topological_sort().unwrap();
    let positions: HashMap<String, usize> = sorted
        .iter()
        .enumerate()
        .map(|(i, node)| (node.name.clone(), i))
        .collect();

    assert!(positions["build.deps"] < positions["test"]);
    assert!(positions["build.compile"] < positions["test"]);
}

#[test]
fn test_missing_dependency() {
    let mut graph = TaskGraph::new();

    // Create task with dependency that doesn't exist
    let task = TestTask::new(&["missing"]);
    graph.add_task("dependent", task).unwrap();

    // Should fail to add edges due to missing dependency
    assert!(graph.add_dependency_edges().is_err());
}

#[test]
fn test_empty_graph() {
    let graph: TaskGraph<TestTask> = TaskGraph::new();

    assert_eq!(graph.task_count(), 0);
    assert!(!graph.has_cycles());

    let groups = graph.get_parallel_groups().unwrap();
    assert!(groups.is_empty());
}

#[test]
fn test_diamond_dependency() {
    let mut graph = TaskGraph::new();

    // Create a diamond dependency pattern:
    //     A
    //    / \
    //   B   C
    //    \ /
    //     D
    let task_a = TestTask::new(&[]);
    let task_b = TestTask::new(&["a"]);
    let task_c = TestTask::new(&["a"]);
    let task_d = TestTask::new(&["b", "c"]);

    graph.add_task("a", task_a).unwrap();
    graph.add_task("b", task_b).unwrap();
    graph.add_task("c", task_c).unwrap();
    graph.add_task("d", task_d).unwrap();
    graph.add_dependency_edges().unwrap();

    assert!(!graph.has_cycles());
    assert_eq!(graph.task_count(), 4);

    let groups = graph.get_parallel_groups().unwrap();

    // Should have 3 levels: [A], [B,C], [D]
    assert_eq!(groups.len(), 3);
    assert_eq!(groups[0].len(), 1); // A
    assert_eq!(groups[1].len(), 2); // B and C can run in parallel
    assert_eq!(groups[2].len(), 1); // D
}

#[test]
fn test_self_dependency_cycle() {
    let mut graph = TaskGraph::new();

    // Create self-referencing task
    let task = TestTask::new(&["self_ref"]);
    graph.add_task("self_ref", task).unwrap();
    graph.add_dependency_edges().unwrap();

    assert!(graph.has_cycles());
    assert!(graph.get_parallel_groups().is_err());
}

/// Test that shared dependencies appear only once in the DAG.
///
/// When task A and task B both depend on task C, task C should only
/// appear once in the task graph (deduplication).
#[test]
fn test_shared_dependency_deduplication() {
    let mut graph = TaskGraph::new();

    // Create pattern where both A and B depend on C:
    //     C
    //    / \
    //   A   B
    let task_c = TestTask::new(&[]);
    let task_a = TestTask::new(&["c"]);
    let task_b = TestTask::new(&["c"]);

    graph.add_task("c", task_c).unwrap();
    graph.add_task("a", task_a).unwrap();
    graph.add_task("b", task_b).unwrap();
    graph.add_dependency_edges().unwrap();

    // Verify task C appears exactly once in the graph
    assert_eq!(graph.task_count(), 3, "Should have exactly 3 tasks");

    // Count occurrences of task C in the topological sort
    let sorted = graph.topological_sort().unwrap();
    let c_count = sorted.iter().filter(|node| node.name == "c").count();
    assert_eq!(c_count, 1, "Task C should appear exactly once in the DAG");

    // Verify execution order: C comes before both A and B
    let positions: std::collections::HashMap<String, usize> = sorted
        .iter()
        .enumerate()
        .map(|(i, node)| (node.name.clone(), i))
        .collect();
    assert!(positions["c"] < positions["a"], "C should execute before A");
    assert!(positions["c"] < positions["b"], "C should execute before B");

    // Verify parallel groups: C in level 0, A and B in level 1
    let groups = graph.get_parallel_groups().unwrap();
    assert_eq!(groups.len(), 2, "Should have 2 execution levels");
    assert_eq!(groups[0].len(), 1, "Level 0 should have 1 task (C)");
    assert_eq!(groups[0][0].name, "c");
    assert_eq!(groups[1].len(), 2, "Level 1 should have 2 tasks (A and B)");
}

#[test]
fn test_build_for_task() {
    let mut graph = TaskGraph::new();

    // Create a map of available tasks
    let mut all_tasks = HashMap::new();
    all_tasks.insert("a".to_string(), TestTask::new(&[]));
    all_tasks.insert("b".to_string(), TestTask::new(&["a"]));
    all_tasks.insert("c".to_string(), TestTask::new(&["b"]));
    all_tasks.insert("d".to_string(), TestTask::new(&[])); // Not a dependency of c

    // Build graph for "c" - should include a, b, c but not d
    graph
        .build_for_task("c", |name| all_tasks.get(name).cloned())
        .unwrap();

    assert_eq!(graph.task_count(), 3);
    assert!(graph.contains_task("a"));
    assert!(graph.contains_task("b"));
    assert!(graph.contains_task("c"));
    assert!(!graph.contains_task("d"));
}

// Tests for TaskResolver functionality

use crate::{TaskResolution, TaskResolver};

/// Test resolver that supports groups
struct TestResolver {
    tasks: HashMap<String, TestTask>,
    sequential_groups: HashMap<String, Vec<String>>,
    parallel_groups: HashMap<String, (Vec<String>, Vec<String>)>, // (children, depends_on)
}

impl TestResolver {
    fn new() -> Self {
        Self {
            tasks: HashMap::new(),
            sequential_groups: HashMap::new(),
            parallel_groups: HashMap::new(),
        }
    }

    fn add_task(&mut self, name: &str, task: TestTask) {
        self.tasks.insert(name.to_string(), task);
    }

    fn add_sequential_group(&mut self, name: &str, children: &[&str]) {
        self.sequential_groups.insert(
            name.to_string(),
            children.iter().map(|s| (*s).to_string()).collect(),
        );
    }

    fn add_parallel_group(&mut self, name: &str, children: &[&str], depends_on: &[&str]) {
        self.parallel_groups.insert(
            name.to_string(),
            (
                children.iter().map(|s| (*s).to_string()).collect(),
                depends_on.iter().map(|s| (*s).to_string()).collect(),
            ),
        );
    }
}

impl TaskResolver<TestTask> for TestResolver {
    fn resolve(&self, name: &str) -> Option<TaskResolution<TestTask>> {
        // Check if it's a direct task
        if let Some(task) = self.tasks.get(name) {
            return Some(TaskResolution::Single(task.clone()));
        }
        // Check if it's a sequential group
        if let Some(children) = self.sequential_groups.get(name) {
            return Some(TaskResolution::Sequential {
                children: children.clone(),
            });
        }
        // Check if it's a parallel group
        if let Some((children, depends_on)) = self.parallel_groups.get(name) {
            return Some(TaskResolution::Parallel {
                children: children.clone(),
                depends_on: depends_on.clone(),
            });
        }
        None
    }
}

#[test]
fn test_resolver_single_task() {
    let mut resolver = TestResolver::new();
    resolver.add_task("build", TestTask::new(&[]));
    resolver.add_task("test", TestTask::new(&["build"]));

    let mut graph = TaskGraph::new();
    graph
        .build_for_task_with_resolver("test", &resolver)
        .unwrap();

    assert_eq!(graph.task_count(), 2);
    assert!(graph.contains_task("build"));
    assert!(graph.contains_task("test"));

    let sorted = graph.topological_sort().unwrap();
    let positions: HashMap<String, usize> = sorted
        .iter()
        .enumerate()
        .map(|(i, n)| (n.name.clone(), i))
        .collect();

    assert!(positions["build"] < positions["test"]);
}

#[test]
fn test_resolver_sequential_group() {
    let mut resolver = TestResolver::new();
    // Sequential group: build[0] -> build[1] -> build[2]
    resolver.add_sequential_group("build", &["build[0]", "build[1]", "build[2]"]);
    resolver.add_task("build[0]", TestTask::new(&[]));
    resolver.add_task("build[1]", TestTask::new(&[]));
    resolver.add_task("build[2]", TestTask::new(&[]));

    let mut graph = TaskGraph::new();
    graph
        .build_for_task_with_resolver("build", &resolver)
        .unwrap();

    assert_eq!(graph.task_count(), 3);

    let sorted = graph.topological_sort().unwrap();
    let positions: HashMap<String, usize> = sorted
        .iter()
        .enumerate()
        .map(|(i, n)| (n.name.clone(), i))
        .collect();

    // Sequential ordering must be preserved
    assert!(positions["build[0]"] < positions["build[1]"]);
    assert!(positions["build[1]"] < positions["build[2]"]);
}

#[test]
fn test_resolver_parallel_group() {
    let mut resolver = TestResolver::new();
    // Parallel group with children
    resolver.add_parallel_group(
        "build",
        &["build.frontend", "build.backend"],
        &[], // no group-level deps
    );
    resolver.add_task("build.frontend", TestTask::new(&[]));
    resolver.add_task("build.backend", TestTask::new(&[]));

    let mut graph = TaskGraph::new();
    graph
        .build_for_task_with_resolver("build", &resolver)
        .unwrap();

    assert_eq!(graph.task_count(), 2);
    assert!(graph.contains_task("build.frontend"));
    assert!(graph.contains_task("build.backend"));

    // Both should be at same level (can run in parallel)
    let groups = graph.get_parallel_groups().unwrap();
    assert_eq!(groups.len(), 1); // Single level
    assert_eq!(groups[0].len(), 2); // Both tasks
}

#[test]
fn test_resolver_parallel_group_with_depends_on() {
    let mut resolver = TestResolver::new();
    // Setup task first
    resolver.add_task("setup", TestTask::new(&[]));
    // Parallel group with group-level depends_on
    resolver.add_parallel_group(
        "build",
        &["build.frontend", "build.backend"],
        &["setup"], // group depends on setup
    );
    resolver.add_task("build.frontend", TestTask::new(&[]));
    resolver.add_task("build.backend", TestTask::new(&[]));

    let mut graph = TaskGraph::new();
    graph
        .build_for_task_with_resolver("build", &resolver)
        .unwrap();

    assert_eq!(graph.task_count(), 3);

    let sorted = graph.topological_sort().unwrap();
    let positions: HashMap<String, usize> = sorted
        .iter()
        .enumerate()
        .map(|(i, n)| (n.name.clone(), i))
        .collect();

    // Setup must come before both children
    assert!(positions["setup"] < positions["build.frontend"]);
    assert!(positions["setup"] < positions["build.backend"]);
}

#[test]
fn test_resolver_nested_groups() {
    let mut resolver = TestResolver::new();
    // Top level parallel group
    resolver.add_parallel_group("build", &["build.frontend", "build.backend"], &[]);
    // Nested sequential group
    resolver.add_sequential_group(
        "build.frontend",
        &["build.frontend[0]", "build.frontend[1]"],
    );
    resolver.add_task("build.frontend[0]", TestTask::new(&[]));
    resolver.add_task("build.frontend[1]", TestTask::new(&[]));
    resolver.add_task("build.backend", TestTask::new(&[]));

    let mut graph = TaskGraph::new();
    graph
        .build_for_task_with_resolver("build", &resolver)
        .unwrap();

    assert_eq!(graph.task_count(), 3);

    let sorted = graph.topological_sort().unwrap();
    let positions: HashMap<String, usize> = sorted
        .iter()
        .enumerate()
        .map(|(i, n)| (n.name.clone(), i))
        .collect();

    // Sequential ordering within frontend must be preserved
    assert!(positions["build.frontend[0]"] < positions["build.frontend[1]"]);
}

// ==========================================================================
// compute_affected tests
// ==========================================================================

#[test]
fn test_compute_affected_direct() {
    let mut graph = TaskGraph::new();
    graph.add_task("build", TestTask::new(&[])).unwrap();
    graph.add_task("test", TestTask::new(&["build"])).unwrap();
    graph.add_task("deploy", TestTask::new(&["test"])).unwrap();
    graph.add_dependency_edges().unwrap();

    // Only build is directly affected
    let affected = graph.compute_affected(
        &["build", "test", "deploy"],
        |task| {
            // Simulate: build has no deps (directly affected), others don't
            task.depends_on.is_empty()
        },
        None::<fn(&str) -> bool>,
    );

    // build is directly affected, test and deploy are transitively affected
    assert_eq!(affected, vec!["build", "test", "deploy"]);
}

#[test]
fn test_compute_affected_none() {
    let mut graph = TaskGraph::new();
    graph.add_task("build", TestTask::new(&[])).unwrap();
    graph.add_task("test", TestTask::new(&["build"])).unwrap();
    graph.add_dependency_edges().unwrap();

    // Nothing is directly affected
    let affected =
        graph.compute_affected(&["build", "test"], |_task| false, None::<fn(&str) -> bool>);

    assert!(affected.is_empty());
}

#[test]
fn test_compute_affected_preserves_pipeline_order() {
    let mut graph = TaskGraph::new();
    graph.add_task("deploy", TestTask::new(&["test"])).unwrap();
    graph.add_task("test", TestTask::new(&["build"])).unwrap();
    graph.add_task("build", TestTask::new(&[])).unwrap();
    graph.add_dependency_edges().unwrap();

    // All directly affected
    let affected = graph.compute_affected(
        &["build", "test", "deploy"],
        |_| true,
        None::<fn(&str) -> bool>,
    );

    // Should preserve pipeline order, not graph order
    assert_eq!(affected, vec!["build", "test", "deploy"]);
}

#[test]
fn test_compute_affected_transitive_only() {
    let mut graph = TaskGraph::new();
    graph.add_task("build", TestTask::new(&[])).unwrap();
    graph.add_task("test", TestTask::new(&["build"])).unwrap();
    graph.add_task("deploy", TestTask::new(&["test"])).unwrap();
    graph.add_dependency_edges().unwrap();

    // Only test is directly affected, but deploy depends on it
    let affected = graph.compute_affected(
        &["build", "test", "deploy"],
        |task| {
            // Only "test" has exactly one dependency
            task.depends_on.len() == 1 && task.depends_on[0] == "build"
        },
        None::<fn(&str) -> bool>,
    );

    // test is directly affected, deploy is transitively affected
    // build is not affected because nothing depends on what build does
    assert_eq!(affected, vec!["test", "deploy"]);
}

#[test]
fn test_compute_affected_with_external_resolver() {
    let mut graph = TaskGraph::new();
    // build depends on an external project task, test depends on build
    graph
        .add_task("build", TestTask::new(&["#external:lib"]))
        .unwrap();
    graph.add_task("test", TestTask::new(&["build"])).unwrap();
    // Don't call add_dependency_edges() - external deps would fail validation
    // We manually add the internal edge
    let build_idx = *graph.name_to_node.get("build").unwrap();
    let test_idx = *graph.name_to_node.get("test").unwrap();
    graph.add_edge(build_idx, test_idx);

    // External resolver: #external:lib is affected
    let affected = graph.compute_affected(
        &["build", "test"],
        |_task| false, // Nothing directly affected
        Some(|dep: &str| dep == "#external:lib"),
    );

    // build is affected via external dep, test is transitively affected
    assert_eq!(affected, vec!["build", "test"]);
}

#[test]
fn test_compute_affected_external_not_affected() {
    let mut graph = TaskGraph::new();
    graph
        .add_task("build", TestTask::new(&["#external:lib"]))
        .unwrap();
    graph.add_task("test", TestTask::new(&["build"])).unwrap();
    // Don't call add_dependency_edges() - external deps would fail validation
    let build_idx = *graph.name_to_node.get("build").unwrap();
    let test_idx = *graph.name_to_node.get("test").unwrap();
    graph.add_edge(build_idx, test_idx);

    // External resolver: nothing is affected
    let affected =
        graph.compute_affected(&["build", "test"], |_task| false, Some(|_dep: &str| false));

    assert!(affected.is_empty());
}

// ==========================================================================
// compute_transitive_closure tests
// ==========================================================================

#[test]
fn test_transitive_closure_empty() {
    let deps: std::collections::HashMap<&str, Vec<String>> = std::collections::HashMap::new();
    let closure = compute_transitive_closure(std::iter::empty::<&str>(), |name| {
        deps.get(name).map(|v| v.as_slice())
    });
    assert!(closure.is_empty());
}

#[test]
fn test_transitive_closure_single_node_no_deps() {
    let deps: std::collections::HashMap<&str, Vec<String>> =
        [("build", vec![])].into_iter().collect();
    let closure =
        compute_transitive_closure(["build"], |name| deps.get(name).map(|v| v.as_slice()));
    assert_eq!(closure.len(), 1);
    assert!(closure.contains("build"));
}

#[test]
fn test_transitive_closure_chain() {
    // deploy -> test -> build
    let deps: std::collections::HashMap<&str, Vec<String>> = [
        ("build", vec![]),
        ("test", vec!["build".to_string()]),
        ("deploy", vec!["test".to_string()]),
    ]
    .into_iter()
    .collect();

    let closure =
        compute_transitive_closure(["deploy"], |name| deps.get(name).map(|v| v.as_slice()));

    assert_eq!(closure.len(), 3);
    assert!(closure.contains("deploy"));
    assert!(closure.contains("test"));
    assert!(closure.contains("build"));
}

#[test]
fn test_transitive_closure_diamond() {
    //      A
    //     / \
    //    B   C
    //     \ /
    //      D
    let deps: std::collections::HashMap<&str, Vec<String>> = [
        ("D", vec![]),
        ("B", vec!["D".to_string()]),
        ("C", vec!["D".to_string()]),
        ("A", vec!["B".to_string(), "C".to_string()]),
    ]
    .into_iter()
    .collect();

    let closure = compute_transitive_closure(["A"], |name| deps.get(name).map(|v| v.as_slice()));

    assert_eq!(closure.len(), 4);
    assert!(closure.contains("A"));
    assert!(closure.contains("B"));
    assert!(closure.contains("C"));
    assert!(closure.contains("D"));
}

#[test]
fn test_transitive_closure_multiple_initial() {
    // Two separate chains: A -> B, C -> D
    let deps: std::collections::HashMap<&str, Vec<String>> = [
        ("B", vec![]),
        ("A", vec!["B".to_string()]),
        ("D", vec![]),
        ("C", vec!["D".to_string()]),
    ]
    .into_iter()
    .collect();

    let closure =
        compute_transitive_closure(["A", "C"], |name| deps.get(name).map(|v| v.as_slice()));

    assert_eq!(closure.len(), 4);
    assert!(closure.contains("A"));
    assert!(closure.contains("B"));
    assert!(closure.contains("C"));
    assert!(closure.contains("D"));
}

#[test]
fn test_transitive_closure_missing_dep() {
    // A depends on nonexistent B - should just include A
    let deps: std::collections::HashMap<&str, Vec<String>> =
        [("A", vec!["B".to_string()])].into_iter().collect();

    let closure = compute_transitive_closure(["A"], |name| deps.get(name).map(|v| v.as_slice()));

    // A is included, B is added to closure even though it has no entry (it's a valid node name)
    assert_eq!(closure.len(), 2);
    assert!(closure.contains("A"));
    assert!(closure.contains("B"));
}

// ========================================================================
// NodeKind tests
// ========================================================================

#[test]
fn test_node_kind_default() {
    let kind = NodeKind::default();
    assert_eq!(kind, NodeKind::Task);
}

#[test]
fn test_node_kind_display() {
    assert_eq!(NodeKind::Task.to_string(), "task");
    assert_eq!(NodeKind::Service.to_string(), "service");
}

#[test]
fn test_node_kind_equality() {
    assert_eq!(NodeKind::Task, NodeKind::Task);
    assert_eq!(NodeKind::Service, NodeKind::Service);
    assert_ne!(NodeKind::Task, NodeKind::Service);
}

#[test]
fn test_add_task_sets_kind() {
    let mut graph = TaskGraph::new();
    graph.add_task("build", TestTask::new(&[])).unwrap();

    let node = graph.get_node_by_name("build").unwrap();
    assert_eq!(node.kind, NodeKind::Task);
}

#[test]
fn test_add_service_sets_kind() {
    let mut graph = TaskGraph::new();
    graph.add_service("db", TestTask::new(&[])).unwrap();

    let node = graph.get_node_by_name("db").unwrap();
    assert_eq!(node.kind, NodeKind::Service);
}

#[test]
fn test_mixed_graph_tasks_and_services() {
    let mut graph = TaskGraph::new();

    // Add a task and a service
    graph.add_task("build", TestTask::new(&[])).unwrap();
    graph.add_service("db", TestTask::new(&["build"])).unwrap();
    graph.add_dependency_edges().unwrap();

    assert_eq!(graph.task_count(), 2);
    assert!(!graph.has_cycles());

    // Verify kinds
    let build_node = graph.get_node_by_name("build").unwrap();
    assert_eq!(build_node.kind, NodeKind::Task);

    let db_node = graph.get_node_by_name("db").unwrap();
    assert_eq!(db_node.kind, NodeKind::Service);

    // Verify topological order
    let sorted = graph.topological_sort().unwrap();
    let positions: HashMap<String, usize> = sorted
        .iter()
        .enumerate()
        .map(|(i, node)| (node.name.clone(), i))
        .collect();
    assert!(positions["build"] < positions["db"]);
}

#[test]
fn test_add_service_deduplication() {
    let mut graph = TaskGraph::new();
    let idx1 = graph.add_service("db", TestTask::new(&[])).unwrap();
    let idx2 = graph.add_service("db", TestTask::new(&[])).unwrap();
    assert_eq!(idx1, idx2);
    assert_eq!(graph.task_count(), 1);
}

#[test]
fn test_duplicate_node_name_across_kinds() {
    let mut graph = TaskGraph::new();
    graph.add_task("api", TestTask::new(&[])).unwrap();

    let err = graph
        .add_image("api", TestTask::new(&[]))
        .expect_err("should reject image with same name as task");
    assert!(
        matches!(err, Error::DuplicateNodeName { ref name, .. } if name == "api"),
        "expected DuplicateNodeName error, got: {err}"
    );

    // Reverse direction: image first, then task
    let mut graph2 = TaskGraph::new();
    graph2.add_image("worker", TestTask::new(&[])).unwrap();

    let err2 = graph2
        .add_service("worker", TestTask::new(&[]))
        .expect_err("should reject service with same name as image");
    assert!(
        matches!(err2, Error::DuplicateNodeName { ref name, .. } if name == "worker"),
        "expected DuplicateNodeName error, got: {err2}"
    );
}

#[test]
fn test_add_image_deduplication() {
    let mut graph = TaskGraph::new();
    let idx1 = graph.add_image("api", TestTask::new(&[])).unwrap();
    let idx2 = graph.add_image("api", TestTask::new(&[])).unwrap();
    assert_eq!(idx1, idx2);
    assert_eq!(graph.task_count(), 1);
}
