//! Task discovery and label-based filtering
//!
//! Handles task discovery within CUE modules, label matching for task selection,
//! and CUE manifest evaluation with caching support.

use std::path::Path;

use cuengine::ModuleEvalOptions;
use cuenv_core::manifest::Project;
use cuenv_core::tasks::{TaskDefinition, Tasks};
use cuenv_core::ModuleEvaluation;
use cuenv_core::Result;

use crate::commands::env_file::find_cue_module_root;
use crate::commands::{convert_engine_error, relative_path_from_root, CommandExecutor};

/// Normalize a list of labels by sorting, deduplicating, and filtering empty strings.
///
/// This ensures consistent behavior across label matching and naming operations.
pub fn normalize_labels(labels: &[String]) -> Vec<String> {
    let mut normalized: Vec<String> = labels
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    normalized.sort();
    normalized.dedup();
    normalized
}

/// Find all tasks that match ALL of the given labels (AND semantics).
///
/// Returns a sorted list of task FQDNs (or names) that have all required labels.
/// Tasks must be `Single` tasks with labels that include every label in the input.
pub fn find_tasks_with_labels(tasks: &Tasks, labels: &[String]) -> Vec<String> {
    let required_labels = normalize_labels(labels);

    let mut matching: Vec<String> = tasks
        .tasks
        .iter()
        .filter_map(|(name, definition)| match definition {
            TaskDefinition::Single(task)
                if required_labels
                    .iter()
                    .all(|label| task.labels.contains(label)) =>
            {
                Some(name.clone())
            }
            _ => None,
        })
        .collect();

    matching.sort();
    matching
}

/// Generate a deterministic synthetic task name for label-based execution.
///
/// The name uses a reserved prefix (`__cuenv_labels__`) to avoid collisions with
/// user-defined task names. The labels are sorted and joined with `+` for stability.
pub fn format_label_root(labels: &[String]) -> String {
    let sorted = normalize_labels(labels);
    format!("__cuenv_labels__{}", sorted.join("+"))
}

/// Evaluate a CUE manifest using module-wide evaluation.
///
/// This function evaluates the entire CUE module once and extracts the Project
/// configuration at the specified directory. It provides helpful error messages
/// when the config uses Base schema instead of Project (tasks require Project).
///
/// When an `executor` is provided, uses its cached module evaluation.
/// Otherwise, falls back to fresh evaluation (legacy behavior).
pub fn evaluate_manifest(
    dir: &Path,
    package: &str,
    executor: Option<&CommandExecutor>,
) -> Result<Project> {
    let target_path = dir.canonicalize().map_err(|e| cuenv_core::Error::Io {
        source: e,
        path: Some(dir.to_path_buf().into_boxed_path()),
        operation: "canonicalize path".to_string(),
    })?;

    // Use executor's cached module if available
    if let Some(exec) = executor {
        tracing::debug!("Using cached module evaluation from executor");
        let module = exec.get_module(&target_path)?;
        let rel_path = relative_path_from_root(&module.root, &target_path);

        let instance = module.get(&rel_path).ok_or_else(|| {
            cuenv_core::Error::configuration(format!(
                "No CUE instance found at path: {} (relative: {})",
                target_path.display(),
                rel_path.display()
            ))
        })?;

        // Check if this is a Project (has name field) or Base (no name)
        return match instance.kind {
            cuenv_core::InstanceKind::Project => instance.deserialize(),
            cuenv_core::InstanceKind::Base => {
                // Valid Base config, but this command needs Project
                Err(cuenv_core::Error::configuration(
                    "This directory uses schema.#Base which doesn't support tasks.\n\
                     To use tasks, update your env.cue to use schema.#Project:\n\n\
                     schema.#Project\n\
                     name: \"your-project-name\"",
                ))
            }
        };
    }

    // Legacy path: fresh evaluation
    tracing::debug!("Using fresh module evaluation (no executor)");

    let module_root = find_cue_module_root(&target_path).ok_or_else(|| {
        cuenv_core::Error::configuration(format!(
            "No CUE module found (looking for cue.mod/) starting from: {}",
            target_path.display()
        ))
    })?;

    let options = ModuleEvalOptions {
        recursive: true,
        ..Default::default()
    };
    let raw_result = cuengine::evaluate_module(&module_root, package, Some(&options))
        .map_err(convert_engine_error)?;

    let module = ModuleEvaluation::from_raw(
        module_root.clone(),
        raw_result.instances,
        raw_result.projects,
    );

    let rel_path = relative_path_from_root(&module_root, &target_path);
    let instance = module.get(&rel_path).ok_or_else(|| {
        cuenv_core::Error::configuration(format!(
            "No CUE instance found at path: {} (relative: {})",
            target_path.display(),
            rel_path.display()
        ))
    })?;

    // Check if this is a Project (has name field) or Base (no name)
    match instance.kind {
        cuenv_core::InstanceKind::Project => instance.deserialize(),
        cuenv_core::InstanceKind::Base => {
            // Valid Base config, but this command needs Project
            Err(cuenv_core::Error::configuration(
                "This directory uses schema.#Base which doesn't support tasks.\n\
                 To use tasks, update your env.cue to use schema.#Project:\n\n\
                 schema.#Project\n\
                 name: \"your-project-name\"",
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_labels_basic() {
        let labels = vec!["b".to_string(), "a".to_string(), "c".to_string()];
        let normalized = normalize_labels(&labels);
        assert_eq!(normalized, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_normalize_labels_deduplicates() {
        let labels = vec!["test".to_string(), "build".to_string(), "test".to_string()];
        let normalized = normalize_labels(&labels);
        assert_eq!(normalized, vec!["build", "test"]);
    }

    #[test]
    fn test_normalize_labels_filters_empty() {
        let labels = vec![
            String::new(),
            "valid".to_string(),
            "   ".to_string(),
            "another".to_string(),
        ];
        let normalized = normalize_labels(&labels);
        assert_eq!(normalized, vec!["another", "valid"]);
    }

    #[test]
    fn test_normalize_labels_trims_whitespace() {
        let labels = vec!["  test  ".to_string(), " build".to_string()];
        let normalized = normalize_labels(&labels);
        assert_eq!(normalized, vec!["build", "test"]);
    }

    #[test]
    fn test_normalize_labels_all_empty_returns_empty() {
        let labels = vec![String::new(), "   ".to_string()];
        let normalized = normalize_labels(&labels);
        assert!(normalized.is_empty());
    }

    #[test]
    fn test_format_label_root() {
        let labels = vec!["build".to_string(), "test".to_string()];
        let root = format_label_root(&labels);
        assert_eq!(root, "__cuenv_labels__build+test");
    }

    #[test]
    fn test_format_label_root_sorts() {
        let labels = vec!["z".to_string(), "a".to_string(), "m".to_string()];
        let root = format_label_root(&labels);
        assert_eq!(root, "__cuenv_labels__a+m+z");
    }
}
