//! Task discovery across monorepo workspaces
//!
//! This module provides functionality to discover tasks across a monorepo,
//! supporting TaskRef resolution and TaskMatcher-based task discovery.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use regex::Regex;
use walkdir::WalkDir;

use crate::manifest::{ArgMatcher, Cuenv, TaskMatcher, TaskRef};
use crate::tasks::Task;

/// A discovered project in the workspace
#[derive(Debug, Clone)]
pub struct DiscoveredProject {
    /// Path to the env.cue file
    pub env_cue_path: PathBuf,
    /// Path to the project root (directory containing env.cue)
    pub project_root: PathBuf,
    /// The parsed manifest
    pub manifest: Cuenv,
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

/// Function type for evaluating env.cue files
pub type EvalFn = Box<dyn Fn(&Path) -> Result<Cuenv, String> + Send + Sync>;

/// Discovers tasks across a monorepo workspace
pub struct TaskDiscovery {
    /// Root directory of the workspace
    workspace_root: PathBuf,
    /// Cached project index: name -> project
    name_index: HashMap<String, DiscoveredProject>,
    /// All discovered projects
    projects: Vec<DiscoveredProject>,
    /// Function to evaluate env.cue files
    eval_fn: Option<EvalFn>,
}

impl TaskDiscovery {
    /// Create a new TaskDiscovery for the given workspace root
    pub fn new(workspace_root: PathBuf) -> Self {
        Self {
            workspace_root,
            name_index: HashMap::new(),
            projects: Vec::new(),
            eval_fn: None,
        }
    }

    /// Set the evaluation function for loading env.cue files
    pub fn with_eval_fn(mut self, eval_fn: EvalFn) -> Self {
        self.eval_fn = Some(eval_fn);
        self
    }

    /// Discover all projects in the workspace
    ///
    /// This scans for env.cue files and builds the name -> project index.
    /// Requires an eval function to be set via `with_eval_fn`.
    pub fn discover(&mut self) -> Result<(), DiscoveryError> {
        self.projects.clear();
        self.name_index.clear();

        let eval_fn = self
            .eval_fn
            .as_ref()
            .ok_or(DiscoveryError::NoEvalFunction)?;

        // Walk the directory tree looking for env.cue files
        for entry in WalkDir::new(&self.workspace_root)
            .follow_links(true)
            .into_iter()
            .filter_entry(|e| !is_hidden(e) && !is_excluded(e))
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            if path.file_name() == Some("env.cue".as_ref()) {
                if let Ok(project) = self.load_project(path, eval_fn) {
                    // Build name index
                    if let Some(name) = &project.manifest.name {
                        self.name_index.insert(name.clone(), project.clone());
                    }
                    self.projects.push(project);
                }
            }
        }

        Ok(())
    }

    /// Add a pre-loaded project to the discovery
    ///
    /// This is useful when you already have a Cuenv manifest loaded.
    pub fn add_project(&mut self, project_root: PathBuf, manifest: Cuenv) {
        let env_cue_path = project_root.join("env.cue");
        let project = DiscoveredProject {
            env_cue_path,
            project_root,
            manifest: manifest.clone(),
        };

        // Build name index
        if let Some(name) = &manifest.name {
            self.name_index.insert(name.clone(), project.clone());
        }
        self.projects.push(project);
    }

    /// Load a single project from its env.cue path
    fn load_project(
        &self,
        env_cue_path: &Path,
        eval_fn: &EvalFn,
    ) -> Result<DiscoveredProject, DiscoveryError> {
        let project_root = env_cue_path
            .parent()
            .ok_or_else(|| DiscoveryError::InvalidPath(env_cue_path.to_path_buf()))?
            .to_path_buf();

        // Use provided eval function to evaluate the env.cue file
        let manifest = eval_fn(&project_root)
            .map_err(|e| DiscoveryError::EvalError(env_cue_path.to_path_buf(), e))?;

        Ok(DiscoveredProject {
            env_cue_path: env_cue_path.to_path_buf(),
            project_root,
            manifest,
        })
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

        let task_def = project
            .manifest
            .tasks
            .get(&task_name)
            .ok_or_else(|| DiscoveryError::TaskNotFound(project_name.clone(), task_name.clone()))?;

        // We only support single tasks, not task groups, for TaskRef
        let task = task_def
            .as_single()
            .ok_or_else(|| {
                DiscoveryError::TaskIsGroup(project_name.clone(), task_name.clone())
            })?
            .clone();

        Ok(MatchedTask {
            project_root: project.project_root.clone(),
            task_name,
            task,
            project_name: project.manifest.name.clone(),
        })
    }

    /// Find all tasks matching a TaskMatcher
    pub fn match_tasks(&self, matcher: &TaskMatcher) -> Vec<MatchedTask> {
        let mut matches = Vec::new();

        for project in &self.projects {
            // Filter by workspace membership if specified
            if let Some(required_workspaces) = &matcher.workspaces {
                if let Some(project_workspaces) = &project.manifest.workspaces {
                    let in_workspace = required_workspaces
                        .iter()
                        .any(|ws| project_workspaces.contains_key(ws));
                    if !in_workspace {
                        continue;
                    }
                } else {
                    // Project has no workspaces defined, skip if we require specific ones
                    continue;
                }
            }

            // Check each task in the project
            for (task_name, task_def) in &project.manifest.tasks {
                // Only match single tasks, not groups
                let Some(task) = task_def.as_single() else {
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
                if let Some(required_command) = &matcher.command {
                    if &task.command != required_command {
                        continue;
                    }
                }

                // Match by args
                if let Some(arg_matchers) = &matcher.args {
                    if !matches_args(&task.args, arg_matchers) {
                        continue;
                    }
                }

                matches.push(MatchedTask {
                    project_root: project.project_root.clone(),
                    task_name: task_name.clone(),
                    task: task.clone(),
                    project_name: project.manifest.name.clone(),
                });
            }
        }

        matches
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

/// Check if task args match all arg matchers
fn matches_args(args: &[String], matchers: &[ArgMatcher]) -> bool {
    for matcher in matchers {
        let matched = args.iter().any(|arg| {
            // Check contains
            if let Some(substring) = &matcher.contains {
                if arg.contains(substring) {
                    return true;
                }
            }
            // Check regex matches
            if let Some(pattern) = &matcher.matches {
                if let Ok(regex) = Regex::new(pattern) {
                    if regex.is_match(arg) {
                        return true;
                    }
                }
            }
            false
        });
        if !matched {
            return false;
        }
    }
    true
}

/// Check if a directory entry is hidden (starts with .)
fn is_hidden(entry: &walkdir::DirEntry) -> bool {
    entry
        .file_name()
        .to_str()
        .map(|s| s.starts_with('.'))
        .unwrap_or(false)
}

/// Check if a directory should be excluded from discovery
fn is_excluded(entry: &walkdir::DirEntry) -> bool {
    let name = entry.file_name().to_str().unwrap_or("");
    matches!(
        name,
        "node_modules" | "target" | "dist" | "build" | ".git" | "vendor"
    )
}

/// Errors that can occur during task discovery
#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    #[error("Invalid path: {0}")]
    InvalidPath(PathBuf),

    #[error("Failed to evaluate {0}: {1}")]
    EvalError(PathBuf, String),

    #[error("Invalid TaskRef format: {0}")]
    InvalidTaskRef(String),

    #[error("Project not found: {0}")]
    ProjectNotFound(String),

    #[error("Task not found: {0}:{1}")]
    TaskNotFound(String, String),

    #[error("Task {0}:{1} is a group, not a single task")]
    TaskIsGroup(String, String),

    #[error("No evaluation function provided - use with_eval_fn()")]
    NoEvalFunction,

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
