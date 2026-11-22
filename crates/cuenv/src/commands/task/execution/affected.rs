//! Local affected-task filtering for `cuenv task --affected [--from <REF>]`.
//!
//! Reuses `cuenv_ci`'s `gix`-based changed-file detection and `compute_affected_tasks`
//! engine so local runs follow the exact same fail-closed semantics as `cuenv ci`.

use super::context::TaskExecutionContext;
use cuenv_ci::affected::{AffectedProjects, ChangedInputs, CheckoutAnchors, CheckoutFrame};
use cuenv_ci::configuration::{ConfigurationSources, ProjectInstance};
use cuenv_ci::provider::{CIProvider, ChangedFiles, LocalProvider};
use cuenv_core::Result;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Filter `candidates` down to the tasks affected by changed files.
///
/// Compares against `from` (merge-base with `HEAD`, plus uncommitted changes)
/// when provided, or uncommitted changes vs `HEAD` otherwise.
///
/// Fails closed (returns all `candidates` with a warning) when no comparable
/// base exists or the project lies outside the git checkout.
pub(super) async fn filter_affected_candidates(
    candidates: Vec<String>,
    context: &TaskExecutionContext,
    from: Option<&str>,
) -> Result<Vec<String>> {
    if candidates.is_empty() {
        return Ok(candidates);
    }

    let provider = match from {
        Some(base_ref) => LocalProvider::with_base_ref(base_ref.to_string()),
        None => LocalProvider::detect()
            .ok_or_else(|| cuenv_core::Error::configuration("local VCS provider unavailable"))?,
    };

    let changes = provider.changed_files().await?;
    let changed_files = match changes {
        ChangedFiles::Compared(files) => files,
        ChangedFiles::NoBase(why) => {
            cuenv_events::emit_stderr!(format!("warning: {why}"));
            return Ok(candidates);
        }
    };

    Ok(filter_with_changed_files(
        &candidates,
        context,
        &changed_files,
    ))
}

/// Pure filtering helper given an already-resolved list of checkout-relative
/// changed files (also exercised directly by unit tests).
pub(super) fn filter_with_changed_files(
    candidates: &[String],
    context: &TaskExecutionContext,
    changed_files: &[PathBuf],
) -> Vec<String> {
    let frame = CheckoutFrame::around(&context.project_root);
    let Some(project_rel) = frame.place(&context.project_root) else {
        cuenv_events::emit_stderr!(format!(
            "warning: project {} is outside the checkout at {}, so its inputs cannot \
             be compared with changes; selecting all candidate tasks",
            context.project_root.display(),
            frame.root().display()
        ));
        return candidates.to_vec();
    };

    let module_prefix = context
        .cue_module_root
        .as_deref()
        .and_then(|root| frame.place(root));

    let mut changed_configuration = HashSet::new();
    if let (Some(module_root), Some(prefix)) =
        (context.cue_module_root.as_deref(), module_prefix.as_deref())
        && let Ok(sources) = ConfigurationSources::load(ProjectInstance {
            module_root,
            directory: &context.project_root,
            module_prefix: prefix,
        })
        && sources.changed_by(changed_files)
    {
        changed_configuration.insert(project_rel.clone());
    }

    let mut all_projects = HashMap::new();
    all_projects.insert(
        context.manifest.name.clone(),
        (project_rel.clone(), context.manifest.clone()),
    );

    let anchors = CheckoutAnchors {
        projects: &all_projects,
        module_root: module_prefix.as_deref().or(Some(Path::new(""))),
    };

    cuenv_ci::affected::compute_affected_tasks(
        ChangedInputs {
            files: changed_files,
            project_root: &project_rel,
            anchors: &anchors,
        },
        candidates,
        &AffectedProjects {
            project: &context.manifest,
            all_projects: &all_projects,
            checkout_root: Some(frame.root()),
            changed_configuration: Some(&changed_configuration),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_core::manifest::Project;
    use cuenv_core::tasks::{Input, Task, TaskDependency, TaskNode};
    use cuenv_task_exec::TaskIndex;
    use tempfile::TempDir;

    fn test_context(root: &Path, manifest: Project) -> TaskExecutionContext {
        let task_index = TaskIndex::build(&manifest.tasks).unwrap();
        let local_tasks = task_index.to_tasks();
        TaskExecutionContext {
            manifest,
            project_root: root.to_path_buf(),
            cue_module_root: None,
            task_index,
            local_tasks,
        }
    }

    #[test]
    fn filters_only_tasks_matching_changed_inputs_and_their_dependents() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().canonicalize().unwrap();

        let mut manifest = Project {
            name: "demo".to_string(),
            ..Default::default()
        };
        manifest.tasks.insert(
            "build_a".to_string(),
            TaskNode::Task(Box::new(Task {
                command: "true".to_string(),
                inputs: vec![Input::Path("a/**".to_string())],
                ..Default::default()
            })),
        );
        manifest.tasks.insert(
            "build_b".to_string(),
            TaskNode::Task(Box::new(Task {
                command: "true".to_string(),
                inputs: vec![Input::Path("b/**".to_string())],
                ..Default::default()
            })),
        );
        manifest.tasks.insert(
            "test_a".to_string(),
            TaskNode::Task(Box::new(Task {
                command: "true".to_string(),
                inputs: vec![Input::Path("tests_a/**".to_string())],
                depends_on: vec![TaskDependency::from_name("build_a")],
                ..Default::default()
            })),
        );

        let context = test_context(&root, manifest);
        let candidates = vec![
            "build_a".to_string(),
            "build_b".to_string(),
            "test_a".to_string(),
        ];

        // Changing a/lib.rs affects build_a and its dependent test_a, but not build_b.
        let affected =
            filter_with_changed_files(&candidates, &context, &[PathBuf::from("a/lib.rs")]);
        assert_eq!(affected, vec!["build_a".to_string(), "test_a".to_string()]);

        // Changing an unrelated file affects none of the tasks with explicit inputs.
        let none = filter_with_changed_files(&candidates, &context, &[PathBuf::from("README.md")]);
        assert_eq!(none, Vec::<String>::new());
    }
}
