//! Task discovery across monorepo workspaces
//!
//! This crate provides functionality to discover tasks across a monorepo,
//! supporting TaskRef resolution and TaskMatcher-based task discovery.

use std::collections::HashMap;
use std::path::PathBuf;

use regex::Regex;

use cuenv_core::manifest::{ArgMatcher, Project, TaskMatcher, TaskRef};
use cuenv_core::tasks::{Task, TaskIndex};

/// A discovered project in the workspace
#[derive(Debug, Clone)]
pub struct DiscoveredProject {
    /// Path to the env.cue file
    pub env_cue_path: PathBuf,
    /// Path to the project root (directory containing env.cue)
    pub project_root: PathBuf,
    /// The parsed manifest
    pub manifest: Project,
}

/// Result of matching a task
#[derive(Debug, Clone)]
pub struct MatchedTask {
    /// Path to the project containing this task
    pub project_root: PathBuf,
    /// Name of the task
    pub task_name: String,
    /// The task definition
    pub task: Task,
    /// Project name (from env.cue name field)
    pub project_name: Option<String>,
}

/// Discovers tasks across a monorepo workspace
pub struct TaskDiscovery {
    /// Cached project index: name -> project
    name_index: HashMap<String, DiscoveredProject>,
    /// All discovered projects
    projects: Vec<DiscoveredProject>,
}

impl TaskDiscovery {
    /// Create a new TaskDiscovery
    ///
    /// The workspace root parameter is kept for API compatibility but unused.
    /// Projects are added explicitly via `add_project()`.
    pub fn new(_workspace_root: PathBuf) -> Self {
        Self {
            name_index: HashMap::new(),
            projects: Vec::new(),
        }
    }

    /// Add a pre-loaded project to the discovery
    ///
    /// This is useful when you already have a Project manifest loaded.
    pub fn add_project(&mut self, project_root: PathBuf, manifest: Project) {
        let env_cue_path = project_root.join("env.cue");
        let project = DiscoveredProject {
            env_cue_path,
            project_root,
            manifest: manifest.clone(),
        };

        // Build name index
        let name = manifest.name.trim();
        if !name.is_empty() {
            self.name_index.insert(name.to_string(), project.clone());
        }
        self.projects.push(project);
    }

    /// Resolve a TaskRef to its actual task definition
    ///
    /// Returns the project root and the task if found
    pub fn resolve_ref(&self, task_ref: &TaskRef) -> Result<MatchedTask, DiscoveryError> {
        let (project_name, task_name) = task_ref
            .parse()
            .ok_or_else(|| DiscoveryError::InvalidTaskRef(task_ref.ref_.clone()))?;

        let project = self
            .name_index
            .get(&project_name)
            .ok_or_else(|| DiscoveryError::ProjectNotFound(project_name.clone()))?;

        let task_def =
            project.manifest.tasks.get(&task_name).ok_or_else(|| {
                DiscoveryError::TaskNotFound(project_name.clone(), task_name.clone())
            })?;

        // We only support single tasks, not task groups, for TaskRef
        let task = task_def
            .as_task()
            .ok_or_else(|| DiscoveryError::TaskIsGroup(project_name.clone(), task_name.clone()))?
            .clone();

        Ok(MatchedTask {
            project_root: project.project_root.clone(),
            task_name,
            task,
            project_name: Some(project.manifest.name.clone()).filter(|s| !s.trim().is_empty()),
        })
    }

    /// Find all tasks matching a TaskMatcher
    ///
    /// Returns an error if any regex pattern in the matcher is invalid.
    pub fn match_tasks(&self, matcher: &TaskMatcher) -> Result<Vec<MatchedTask>, DiscoveryError> {
        // Pre-compile arg matchers to catch regex errors early and avoid recompilation
        let compiled_arg_matchers = match &matcher.args {
            Some(arg_matchers) => Some(compile_arg_matchers(arg_matchers)?),
            None => None,
        };

        let mut matches = Vec::new();

        for project in &self.projects {
            // Use the canonical TaskIndex to include tasks nested in parallel groups.
            let index = TaskIndex::build(&project.manifest.tasks).map_err(|e| {
                DiscoveryError::TaskIndexError(project.env_cue_path.clone(), e.to_string())
            })?;

            // Check each addressable single task in the project
            for entry in index.list() {
                let Some(task) = entry.node.as_task() else {
                    continue;
                };

                // Match by labels
                if let Some(required_labels) = &matcher.labels {
                    let has_all_labels = required_labels
                        .iter()
                        .all(|label| task.labels.contains(label));
                    if !has_all_labels {
                        continue;
                    }
                }

                // Match by command
                if let Some(required_command) = &matcher.command
                    && &task.command != required_command
                {
                    continue;
                }

                // Match by args using pre-compiled matchers
                if let Some(ref compiled) = compiled_arg_matchers
                    && !matches_args_compiled(&task.args, compiled)
                {
                    continue;
                }

                matches.push(MatchedTask {
                    project_root: project.project_root.clone(),
                    task_name: entry.name.clone(),
                    task: task.clone(),
                    project_name: Some(project.manifest.name.clone())
                        .filter(|s| !s.trim().is_empty()),
                });
            }
        }

        Ok(matches)
    }

    /// Get all discovered projects
    pub fn projects(&self) -> &[DiscoveredProject] {
        &self.projects
    }

    /// Get a project by name
    pub fn get_project(&self, name: &str) -> Option<&DiscoveredProject> {
        self.name_index.get(name)
    }
}

/// Compiled version of ArgMatcher for efficient matching
#[derive(Debug)]
struct CompiledArgMatcher {
    contains: Option<String>,
    regex: Option<Regex>,
}

impl CompiledArgMatcher {
    /// Compile an ArgMatcher, validating regex patterns
    fn compile(matcher: &ArgMatcher) -> Result<Self, DiscoveryError> {
        let regex = match &matcher.matches {
            Some(pattern) => {
                // Use regex with size limits to prevent ReDoS
                let regex = regex::RegexBuilder::new(pattern)
                    .size_limit(1024 * 1024) // 1MB compiled size limit
                    .build()
                    .map_err(|e| DiscoveryError::InvalidRegex(pattern.clone(), e.to_string()))?;
                Some(regex)
            }
            None => None,
        };
        Ok(Self {
            contains: matcher.contains.clone(),
            regex,
        })
    }

    /// Check if any argument matches this matcher
    fn matches(&self, args: &[String]) -> bool {
        // If both are None, this matcher matches nothing (conservative behavior)
        if self.contains.is_none() && self.regex.is_none() {
            return false;
        }

        args.iter().any(|arg| {
            if let Some(substring) = &self.contains
                && arg.contains(substring)
            {
                return true;
            }
            if let Some(regex) = &self.regex
                && regex.is_match(arg)
            {
                return true;
            }
            false
        })
    }
}

/// Pre-compile all arg matchers, returning errors for invalid patterns
fn compile_arg_matchers(
    matchers: &[ArgMatcher],
) -> Result<Vec<CompiledArgMatcher>, DiscoveryError> {
    matchers.iter().map(CompiledArgMatcher::compile).collect()
}

/// Check if task args match all arg matchers (using pre-compiled matchers)
fn matches_args_compiled(args: &[String], matchers: &[CompiledArgMatcher]) -> bool {
    matchers.iter().all(|matcher| matcher.matches(args))
}

/// Errors that can occur during task discovery
#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("Invalid path: {0}")]
    InvalidPath(PathBuf),

    #[error("Failed to evaluate {}: {}", .0.display(), .1)]
    EvalError(PathBuf, #[source] Box<cuenv_core::Error>),

    #[error("Invalid TaskRef format: {0}")]
    InvalidTaskRef(String),

    #[error("Project not found: {0}")]
    ProjectNotFound(String),

    #[error("Task not found: {0}:{1}")]
    TaskNotFound(String, String),

    #[error("Task {0}:{1} is a group, not a single task")]
    TaskIsGroup(String, String),

    #[error("Invalid regex pattern '{0}': {1}")]
    InvalidRegex(String, String),

    #[error("Failed to index tasks in {0}: {1}")]
    TaskIndexError(PathBuf, String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_core::tasks::{TaskGroup, TaskNode};
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[test]
    fn test_task_ref_parse() {
        let task_ref = TaskRef {
            ref_: "#projen-generator:bun.install".to_string(),
        };
        let (project, task) = task_ref.parse().unwrap();
        assert_eq!(project, "projen-generator");
        assert_eq!(task, "bun.install");
    }

    #[test]
    fn test_task_ref_parse_invalid() {
        let task_ref = TaskRef {
            ref_: "invalid".to_string(),
        };
        assert!(task_ref.parse().is_none());

        let task_ref = TaskRef {
            ref_: "#no-task".to_string(),
        };
        assert!(task_ref.parse().is_none());
    }

    /// Helper to compile and match for tests
    fn matches_args(args: &[String], matchers: &[ArgMatcher]) -> bool {
        let compiled = compile_arg_matchers(matchers).expect("test matchers should be valid");
        matches_args_compiled(args, &compiled)
    }

    #[test]
    fn test_matches_args_contains() {
        let args = vec!["run".to_string(), ".projenrc.ts".to_string()];
        let matchers = vec![ArgMatcher {
            contains: Some(".projenrc".to_string()),
            matches: None,
        }];
        assert!(matches_args(&args, &matchers));
    }

    #[test]
    fn test_matches_args_regex() {
        let args = vec!["run".to_string(), "test.ts".to_string()];
        let matchers = vec![ArgMatcher {
            contains: None,
            matches: Some(r"\.ts$".to_string()),
        }];
        assert!(matches_args(&args, &matchers));
    }

    #[test]
    fn test_matches_args_no_match() {
        let args = vec!["build".to_string()];
        let matchers = vec![ArgMatcher {
            contains: Some("test".to_string()),
            matches: None,
        }];
        assert!(!matches_args(&args, &matchers));
    }

    #[test]
    fn test_invalid_regex_returns_error() {
        let matchers = vec![ArgMatcher {
            contains: None,
            matches: Some(r"[invalid".to_string()), // Unclosed bracket
        }];
        let result = compile_arg_matchers(&matchers);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, DiscoveryError::InvalidRegex(_, _)));
    }

    #[test]
    fn test_empty_matcher_matches_nothing() {
        let args = vec!["anything".to_string()];
        let matchers = vec![ArgMatcher {
            contains: None,
            matches: None,
        }];
        // Empty matcher should not match anything
        assert!(!matches_args(&args, &matchers));
    }

    #[test]
    fn test_match_tasks_includes_parallel_group_children() {
        let mut discovery = TaskDiscovery::new(PathBuf::from("/tmp"));

        let make_task = || Task {
            command: "echo".into(),
            labels: vec!["projen".into()],
            ..Default::default()
        };

        let mut parallel_tasks = HashMap::new();
        parallel_tasks.insert("generate".into(), TaskNode::Task(Box::new(make_task())));
        parallel_tasks.insert("types".into(), TaskNode::Task(Box::new(make_task())));

        let mut manifest = Project::new("test");
        manifest.tasks.insert(
            "projen".into(),
            TaskNode::Group(TaskGroup {
                type_: "group".to_string(),
                children: parallel_tasks,
                depends_on: Vec::new(),
                description: None,
                max_concurrency: None,
            }),
        );

        discovery.add_project(PathBuf::from("/tmp/proj"), manifest);

        let matcher = TaskMatcher {
            labels: Some(vec!["projen".into()]),
            command: None,
            args: None,
            parallel: true,
        };

        let matches = discovery.match_tasks(&matcher).unwrap();
        let names: Vec<String> = matches.into_iter().map(|m| m.task_name).collect();
        assert_eq!(names.len(), 2);
        assert!(names.contains(&"projen.generate".to_string()));
        assert!(names.contains(&"projen.types".to_string()));
    }

    #[test]
    fn test_task_discovery_new() {
        let discovery = TaskDiscovery::new(PathBuf::from("/workspace"));
        assert!(discovery.projects().is_empty());
        assert!(discovery.get_project("anything").is_none());
    }

    #[test]
    fn test_task_discovery_add_project_with_empty_name() {
        let mut discovery = TaskDiscovery::new(PathBuf::from("/tmp"));

        // Project with empty name should still be added to projects list
        // but not to the name index
        let manifest = Project::new("");
        discovery.add_project(PathBuf::from("/tmp/proj"), manifest);

        assert_eq!(discovery.projects().len(), 1);
        assert!(discovery.get_project("").is_none()); // Empty names not indexed
    }

    #[test]
    fn test_task_discovery_add_project_with_whitespace_name() {
        let mut discovery = TaskDiscovery::new(PathBuf::from("/tmp"));

        // Project with whitespace-only name should not be indexed
        let manifest = Project::new("   ");
        discovery.add_project(PathBuf::from("/tmp/proj"), manifest);

        assert_eq!(discovery.projects().len(), 1);
        assert!(discovery.get_project("   ").is_none());
    }

    #[test]
    fn test_task_discovery_add_project_indexed_by_name() {
        let mut discovery = TaskDiscovery::new(PathBuf::from("/tmp"));

        let manifest = Project::new("my-project");
        discovery.add_project(PathBuf::from("/tmp/proj"), manifest);

        assert_eq!(discovery.projects().len(), 1);
        let project = discovery.get_project("my-project");
        assert!(project.is_some());
        assert_eq!(project.unwrap().project_root, PathBuf::from("/tmp/proj"));
    }

    #[test]
    fn test_task_discovery_resolve_ref_invalid_format() {
        let discovery = TaskDiscovery::new(PathBuf::from("/tmp"));

        let task_ref = TaskRef {
            ref_: "invalid-format".to_string(),
        };
        let result = discovery.resolve_ref(&task_ref);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            DiscoveryError::InvalidTaskRef(_)
        ));
    }

    #[test]
    fn test_task_discovery_resolve_ref_project_not_found() {
        let discovery = TaskDiscovery::new(PathBuf::from("/tmp"));

        let task_ref = TaskRef {
            ref_: "#nonexistent:task".to_string(),
        };
        let result = discovery.resolve_ref(&task_ref);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            DiscoveryError::ProjectNotFound(_)
        ));
    }

    #[test]
    fn test_task_discovery_resolve_ref_task_not_found() {
        let mut discovery = TaskDiscovery::new(PathBuf::from("/tmp"));

        let manifest = Project::new("my-project");
        discovery.add_project(PathBuf::from("/tmp/proj"), manifest);

        let task_ref = TaskRef {
            ref_: "#my-project:nonexistent".to_string(),
        };
        let result = discovery.resolve_ref(&task_ref);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            DiscoveryError::TaskNotFound(_, _)
        ));
    }

    #[test]
    fn test_task_discovery_resolve_ref_task_is_group() {
        let mut discovery = TaskDiscovery::new(PathBuf::from("/tmp"));

        let mut manifest = Project::new("my-project");
        manifest.tasks.insert(
            "group-task".into(),
            TaskNode::Group(TaskGroup {
                type_: "group".to_string(),
                children: HashMap::new(),
                depends_on: Vec::new(),
                description: None,
                max_concurrency: None,
            }),
        );
        discovery.add_project(PathBuf::from("/tmp/proj"), manifest);

        let task_ref = TaskRef {
            ref_: "#my-project:group-task".to_string(),
        };
        let result = discovery.resolve_ref(&task_ref);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            DiscoveryError::TaskIsGroup(_, _)
        ));
    }

    #[test]
    fn test_task_discovery_resolve_ref_success() {
        let mut discovery = TaskDiscovery::new(PathBuf::from("/tmp"));

        let mut manifest = Project::new("my-project");
        manifest.tasks.insert(
            "build".into(),
            TaskNode::Task(Box::new(Task {
                command: "cargo".into(),
                args: vec!["build".into()],
                ..Default::default()
            })),
        );
        discovery.add_project(PathBuf::from("/tmp/proj"), manifest);

        let task_ref = TaskRef {
            ref_: "#my-project:build".to_string(),
        };
        let result = discovery.resolve_ref(&task_ref);
        assert!(result.is_ok());

        let matched = result.unwrap();
        assert_eq!(matched.task_name, "build");
        assert_eq!(matched.project_root, PathBuf::from("/tmp/proj"));
        assert_eq!(matched.project_name, Some("my-project".to_string()));
        assert_eq!(matched.task.command, "cargo");
    }

    #[test]
    fn test_match_tasks_by_command() {
        let mut discovery = TaskDiscovery::new(PathBuf::from("/tmp"));

        let mut manifest = Project::new("test");
        manifest.tasks.insert(
            "build".into(),
            TaskNode::Task(Box::new(Task {
                command: "cargo".into(),
                ..Default::default()
            })),
        );
        manifest.tasks.insert(
            "test".into(),
            TaskNode::Task(Box::new(Task {
                command: "npm".into(),
                ..Default::default()
            })),
        );
        discovery.add_project(PathBuf::from("/tmp/proj"), manifest);

        let matcher = TaskMatcher {
            labels: None,
            command: Some("cargo".into()),
            args: None,
            parallel: false,
        };

        let matches = discovery.match_tasks(&matcher).unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].task_name, "build");
    }

    #[test]
    fn test_match_tasks_by_labels() {
        let mut discovery = TaskDiscovery::new(PathBuf::from("/tmp"));

        let mut manifest = Project::new("test");
        manifest.tasks.insert(
            "task1".into(),
            TaskNode::Task(Box::new(Task {
                command: "echo".into(),
                labels: vec!["ci".into(), "test".into()],
                ..Default::default()
            })),
        );
        manifest.tasks.insert(
            "task2".into(),
            TaskNode::Task(Box::new(Task {
                command: "echo".into(),
                labels: vec!["ci".into()],
                ..Default::default()
            })),
        );
        discovery.add_project(PathBuf::from("/tmp/proj"), manifest);

        // Match tasks with both "ci" and "test" labels
        let matcher = TaskMatcher {
            labels: Some(vec!["ci".into(), "test".into()]),
            command: None,
            args: None,
            parallel: false,
        };

        let matches = discovery.match_tasks(&matcher).unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].task_name, "task1");
    }

    #[test]
    fn test_match_tasks_empty_projects() {
        let discovery = TaskDiscovery::new(PathBuf::from("/tmp"));

        let matcher = TaskMatcher {
            labels: Some(vec!["ci".into()]),
            command: None,
            args: None,
            parallel: false,
        };

        let matches = discovery.match_tasks(&matcher).unwrap();
        assert!(matches.is_empty());
    }

    #[test]
    fn test_matches_args_both_contains_and_regex() {
        let args = vec!["run".to_string(), ".projenrc.ts".to_string()];

        // When both contains and matches are provided, either can match
        let matchers = vec![ArgMatcher {
            contains: Some("notfound".to_string()),
            matches: Some(r"\.ts$".to_string()),
        }];
        assert!(matches_args(&args, &matchers));

        // Check contains matches
        let matchers = vec![ArgMatcher {
            contains: Some(".projenrc".to_string()),
            matches: Some(r"notfound".to_string()),
        }];
        assert!(matches_args(&args, &matchers));
    }

    #[test]
    fn test_matches_args_multiple_matchers() {
        let args = vec![
            "run".to_string(),
            ".projenrc.ts".to_string(),
            "--verbose".to_string(),
        ];

        // All matchers must match
        let matchers = vec![
            ArgMatcher {
                contains: Some(".projenrc".to_string()),
                matches: None,
            },
            ArgMatcher {
                contains: Some("--verbose".to_string()),
                matches: None,
            },
        ];
        assert!(matches_args(&args, &matchers));

        // If one doesn't match, result is false
        let matchers = vec![
            ArgMatcher {
                contains: Some(".projenrc".to_string()),
                matches: None,
            },
            ArgMatcher {
                contains: Some("--quiet".to_string()),
                matches: None,
            },
        ];
        assert!(!matches_args(&args, &matchers));
    }

    #[test]
    fn test_matches_args_empty_args() {
        let args: Vec<String> = vec![];

        let matchers = vec![ArgMatcher {
            contains: Some("anything".to_string()),
            matches: None,
        }];
        assert!(!matches_args(&args, &matchers));
    }

    #[test]
    fn test_matches_args_empty_matchers() {
        let args = vec!["anything".to_string()];
        let matchers: Vec<ArgMatcher> = vec![];

        // Empty matchers list means all tasks match (vacuous truth)
        assert!(matches_args(&args, &matchers));
    }

    #[test]
    fn test_discovery_error_display() {
        let err = DiscoveryError::InvalidTaskRef("bad".to_string());
        assert!(err.to_string().contains("Invalid TaskRef format"));

        let err = DiscoveryError::ProjectNotFound("proj".to_string());
        assert!(err.to_string().contains("Project not found"));

        let err = DiscoveryError::TaskNotFound("proj".to_string(), "task".to_string());
        assert!(err.to_string().contains("Task not found"));

        let err = DiscoveryError::TaskIsGroup("proj".to_string(), "task".to_string());
        assert!(err.to_string().contains("is a group"));

        let err = DiscoveryError::InvalidRegex("bad".to_string(), "error".to_string());
        assert!(err.to_string().contains("Invalid regex"));

        let err = DiscoveryError::InvalidPath(PathBuf::from("/bad"));
        assert!(err.to_string().contains("Invalid path"));

        let err = DiscoveryError::TaskIndexError(PathBuf::from("/env.cue"), "error".to_string());
        assert!(err.to_string().contains("Failed to index"));
    }

    #[test]
    fn test_discovered_project_fields() {
        let project = DiscoveredProject {
            env_cue_path: PathBuf::from("/workspace/env.cue"),
            project_root: PathBuf::from("/workspace"),
            manifest: Project::new("test"),
        };

        assert_eq!(project.env_cue_path, PathBuf::from("/workspace/env.cue"));
        assert_eq!(project.project_root, PathBuf::from("/workspace"));
        assert_eq!(project.manifest.name, "test");
    }

    #[test]
    fn test_matched_task_fields() {
        let matched = MatchedTask {
            project_root: PathBuf::from("/workspace"),
            task_name: "build".to_string(),
            task: Task {
                command: "cargo".into(),
                ..Default::default()
            },
            project_name: Some("my-project".to_string()),
        };

        assert_eq!(matched.project_root, PathBuf::from("/workspace"));
        assert_eq!(matched.task_name, "build");
        assert_eq!(matched.task.command, "cargo");
        assert_eq!(matched.project_name, Some("my-project".to_string()));
    }

    #[test]
    fn test_matched_task_no_project_name() {
        let matched = MatchedTask {
            project_root: PathBuf::from("/workspace"),
            task_name: "build".to_string(),
            task: Task::default(),
            project_name: None,
        };

        assert!(matched.project_name.is_none());
    }
}
