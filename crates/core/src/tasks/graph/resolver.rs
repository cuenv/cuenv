//! Task path resolution for the core task graph wrapper.

use crate::tasks::{Task, TaskNode, Tasks};
use cuenv_task_graph::{TaskResolution, TaskResolver};

impl TaskResolver<Task> for Tasks {
    fn resolve(&self, name: &str) -> Option<TaskResolution<Task>> {
        let node = self.resolve_path(name)?;
        Some(self.node_to_resolution(name, node))
    }
}

impl Tasks {
    /// Walk a dotted/bracketed path to find the TaskNode.
    ///
    /// This method first tries a direct lookup (for flat task names like `bun.setup`),
    /// then falls back to walking the nested path structure.
    ///
    /// Examples:
    /// - `"build"` -> top-level lookup
    /// - `"build.frontend"` -> first tries `tasks["build.frontend"]`, then `tasks["build"].parallel["frontend"]`
    /// - `"build[0]"` -> first tries `tasks["build[0]"]`, then `tasks["build"].steps[0]`
    /// - `"build.frontend[0]"` -> nested: parallel then sequential
    fn resolve_path(&self, path: &str) -> Option<&TaskNode> {
        if let Some(task) = self.tasks.get(path) {
            return Some(task);
        }

        let segments = parse_path_segments(path);
        let root_segment = match segments.first()? {
            PathSegment::Name(name) => name.as_str(),
            PathSegment::Index(_) => return None,
        };

        let mut current = self.tasks.get(root_segment)?;

        for segment in &segments[1..] {
            current = match (current, segment) {
                (TaskNode::Group(group), PathSegment::Name(name)) => group.children.get(name)?,
                (TaskNode::Sequence(steps), PathSegment::Index(idx)) => steps.get(*idx)?,
                _ => return None,
            };
        }

        Some(current)
    }

    fn node_to_resolution(&self, name: &str, node: &TaskNode) -> TaskResolution<Task> {
        match node {
            TaskNode::Task(task) => TaskResolution::Single(task.as_ref().clone()),
            TaskNode::Sequence(steps) => TaskResolution::Sequential {
                children: (0..steps.len()).map(|i| format!("{name}[{i}]")).collect(),
            },
            TaskNode::Group(group) => TaskResolution::Parallel {
                children: group
                    .children
                    .keys()
                    .map(|child| format!("{name}.{child}"))
                    .collect(),
                depends_on: group
                    .depends_on
                    .iter()
                    .map(|dependency| dependency.task_name().to_string())
                    .collect(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PathSegment {
    Name(String),
    Index(usize),
}

fn parse_path_segments(path: &str) -> Vec<PathSegment> {
    let mut segments = Vec::new();
    let mut current_name = String::new();
    let mut chars = path.chars();

    while let Some(c) = chars.next() {
        match c {
            '.' => push_name_segment(&mut segments, &mut current_name),
            '[' => {
                push_name_segment(&mut segments, &mut current_name);
                push_index_segment(&mut segments, &mut chars);
            }
            _ => current_name.push(c),
        }
    }

    push_name_segment(&mut segments, &mut current_name);
    segments
}

fn push_name_segment(segments: &mut Vec<PathSegment>, current_name: &mut String) {
    if !current_name.is_empty() {
        segments.push(PathSegment::Name(std::mem::take(current_name)));
    }
}

fn push_index_segment(segments: &mut Vec<PathSegment>, chars: &mut std::str::Chars<'_>) {
    let mut index = String::new();
    for c in chars.by_ref() {
        if c == ']' {
            break;
        }
        index.push(c);
    }

    if let Ok(idx) = index.parse::<usize>() {
        segments.push(PathSegment::Index(idx));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::{TaskDependency, TaskGroup, TaskNode};
    use crate::test_utils::create_task;
    use std::collections::HashMap;

    #[test]
    fn test_parse_path_segments_simple_name() {
        let segments = parse_path_segments("build");
        assert_eq!(segments, vec![PathSegment::Name("build".into())]);
    }

    #[test]
    fn test_parse_path_segments_dotted() {
        let segments = parse_path_segments("build.frontend");
        assert_eq!(
            segments,
            vec![
                PathSegment::Name("build".into()),
                PathSegment::Name("frontend".into()),
            ]
        );
    }

    #[test]
    fn test_parse_path_segments_indexed() {
        let segments = parse_path_segments("build[0]");
        assert_eq!(
            segments,
            vec![PathSegment::Name("build".into()), PathSegment::Index(0),]
        );
    }

    #[test]
    fn test_parse_path_segments_nested() {
        let segments = parse_path_segments("build.frontend[0]");
        assert_eq!(
            segments,
            vec![
                PathSegment::Name("build".into()),
                PathSegment::Name("frontend".into()),
                PathSegment::Index(0),
            ]
        );
    }

    #[test]
    fn test_task_resolver_single_task() {
        let task = create_task("build", vec![], vec![]);
        let mut tasks = Tasks::new();
        tasks
            .tasks
            .insert("build".into(), TaskNode::Task(Box::new(task)));

        let resolution = tasks.resolve("build");
        assert!(resolution.is_some());
        match resolution.unwrap() {
            TaskResolution::Single(task) => assert_eq!(task.command, "echo build"),
            _ => panic!("Expected Single resolution"),
        }
    }

    #[test]
    fn test_task_resolver_parallel_group() {
        let frontend = create_task("frontend", vec![], vec![]);
        let backend = create_task("backend", vec![], vec![]);

        let mut parallel_tasks = HashMap::new();
        parallel_tasks.insert("frontend".into(), TaskNode::Task(Box::new(frontend)));
        parallel_tasks.insert("backend".into(), TaskNode::Task(Box::new(backend)));

        let group = TaskGroup {
            type_: "group".to_string(),
            children: parallel_tasks,
            depends_on: vec![TaskDependency::from_name("setup")],
            description: None,
            max_concurrency: None,
        };

        let mut tasks = Tasks::new();
        tasks.tasks.insert("build".into(), TaskNode::Group(group));

        let resolution = tasks.resolve("build");
        assert!(resolution.is_some());
        match resolution.unwrap() {
            TaskResolution::Parallel {
                children,
                depends_on,
            } => {
                assert_eq!(children.len(), 2);
                assert!(children.contains(&"build.frontend".to_string()));
                assert!(children.contains(&"build.backend".to_string()));
                assert_eq!(depends_on, vec!["setup"]);
            }
            _ => panic!("Expected Parallel resolution"),
        }
    }

    #[test]
    fn test_task_resolver_sequential_group() {
        let task1 = create_task("t1", vec![], vec![]);
        let task2 = create_task("t2", vec![], vec![]);

        let sequence = TaskNode::Sequence(vec![
            TaskNode::Task(Box::new(task1)),
            TaskNode::Task(Box::new(task2)),
        ]);

        let mut tasks = Tasks::new();
        tasks.tasks.insert("build".into(), sequence);

        let resolution = tasks.resolve("build");
        assert!(resolution.is_some());
        match resolution.unwrap() {
            TaskResolution::Sequential { children } => {
                assert_eq!(children, vec!["build[0]", "build[1]"]);
            }
            _ => panic!("Expected Sequential resolution"),
        }
    }

    #[test]
    fn test_task_resolver_nested_path() {
        let task = create_task("fe", vec![], vec![]);

        let mut parallel_tasks = HashMap::new();
        parallel_tasks.insert("frontend".into(), TaskNode::Task(Box::new(task)));

        let group = TaskGroup {
            type_: "group".to_string(),
            children: parallel_tasks,
            depends_on: vec![],
            description: None,
            max_concurrency: None,
        };

        let mut tasks = Tasks::new();
        tasks.tasks.insert("build".into(), TaskNode::Group(group));

        let resolution = tasks.resolve("build.frontend");
        assert!(resolution.is_some());
        match resolution.unwrap() {
            TaskResolution::Single(task) => assert_eq!(task.command, "echo fe"),
            _ => panic!("Expected Single resolution"),
        }
    }

    #[test]
    fn test_task_resolver_nonexistent() {
        let tasks = Tasks::new();
        assert!(tasks.resolve("nonexistent").is_none());
    }
}
