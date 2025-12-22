//! GitHub Models CLI Extension Stage Contributor
//!
//! Contributes the gh-models extension setup task to CI pipelines that use
//! `gh models` commands for LLM evaluation.
//!
//! Self-detects activation by checking if any task in the project uses
//! the `gh` command with `models` as the first argument.

use super::StageContributor;
use crate::ir::{BuildStage, IntermediateRepresentation, StageTask};
use cuenv_core::manifest::Project;
use cuenv_core::tasks::TaskDefinition;
use std::collections::HashMap;

/// GitHub Models CLI extension stage contributor
///
/// Self-detects activation by checking if the project contains tasks that
/// use `gh models` commands (e.g., for LLM evaluation).
///
/// When active, contributes:
/// - Setup: Install the gh-models extension via `gh extension install github/gh-models`
#[derive(Debug, Clone, Copy, Default)]
pub struct GhModelsContributor;

impl GhModelsContributor {
    /// Check if a task uses `gh models` command
    fn task_uses_gh_models(definition: &TaskDefinition) -> bool {
        match definition {
            TaskDefinition::Single(task) => {
                // Check if command is "gh" and first arg is "models"
                task.command == "gh" && task.args.first().is_some_and(|arg| arg == "models")
            }
            TaskDefinition::Group(group) => {
                // Recursively check group members
                match group {
                    cuenv_core::tasks::TaskGroup::Sequential(tasks) => {
                        tasks.iter().any(Self::task_uses_gh_models)
                    }
                    cuenv_core::tasks::TaskGroup::Parallel(parallel) => {
                        parallel.tasks.values().any(Self::task_uses_gh_models)
                    }
                }
            }
        }
    }

    /// Check if any task in the project uses `gh models`
    fn project_uses_gh_models(project: &Project) -> bool {
        project.tasks.values().any(Self::task_uses_gh_models)
    }
}

impl StageContributor for GhModelsContributor {
    fn id(&self) -> &'static str {
        "gh-models"
    }

    fn is_active(&self, _ir: &IntermediateRepresentation, project: &Project) -> bool {
        // Self-detect: check if the project has tasks using gh models
        Self::project_uses_gh_models(project)
    }

    fn contribute(
        &self,
        ir: &IntermediateRepresentation,
        _project: &Project,
    ) -> (Vec<(BuildStage, StageTask)>, bool) {
        // Idempotency: check if already contributed
        if ir.stages.setup.iter().any(|t| t.id == "setup-gh-models") {
            return (vec![], false);
        }

        (
            vec![(
                BuildStage::Setup,
                StageTask {
                    id: "setup-gh-models".to_string(),
                    provider: "gh-models".to_string(),
                    label: Some("Setup GitHub Models CLI".to_string()),
                    command: vec!["gh extension install github/gh-models".to_string()],
                    shell: false,
                    env: HashMap::new(),
                    depends_on: vec![], // No dependencies - gh should be available
                    priority: 25,       // After cuenv setup but before task execution
                    ..Default::default()
                },
            )],
            true,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{PipelineMetadata, StageConfiguration};
    use cuenv_core::tasks::Task;
    use std::collections::HashMap;

    fn make_ir() -> IntermediateRepresentation {
        IntermediateRepresentation {
            version: "1.4".to_string(),
            pipeline: PipelineMetadata {
                name: "test".to_string(),
                environment: None,
                requires_onepassword: false,
                project_name: None,
                trigger: None,
            },
            runtimes: vec![],
            stages: StageConfiguration::default(),
            tasks: vec![],
        }
    }

    fn make_project_with_gh_models() -> Project {
        let mut project = Project::new("test");
        let mut tasks = HashMap::new();
        tasks.insert(
            "eval.task-gen".to_string(),
            TaskDefinition::Single(Box::new(Task {
                command: "gh".to_string(),
                args: vec![
                    "models".to_string(),
                    "eval".to_string(),
                    "prompts/test.yml".to_string(),
                ],
                ..Default::default()
            })),
        );
        project.tasks = tasks;
        project
    }

    fn make_project_without_gh_models() -> Project {
        let mut project = Project::new("test");
        let mut tasks = HashMap::new();
        tasks.insert(
            "build".to_string(),
            TaskDefinition::Single(Box::new(Task {
                command: "cargo".to_string(),
                args: vec!["build".to_string()],
                ..Default::default()
            })),
        );
        project.tasks = tasks;
        project
    }

    #[test]
    fn test_is_active_with_gh_models_task() {
        let contributor = GhModelsContributor;
        let ir = make_ir();
        let project = make_project_with_gh_models();

        assert!(contributor.is_active(&ir, &project));
    }

    #[test]
    fn test_is_inactive_without_gh_models_task() {
        let contributor = GhModelsContributor;
        let ir = make_ir();
        let project = make_project_without_gh_models();

        assert!(!contributor.is_active(&ir, &project));
    }

    #[test]
    fn test_contribute_returns_setup_task() {
        let contributor = GhModelsContributor;
        let ir = make_ir();
        let project = make_project_with_gh_models();

        let (contributions, modified) = contributor.contribute(&ir, &project);

        assert!(modified);
        assert_eq!(contributions.len(), 1);

        let (stage, task) = &contributions[0];
        assert_eq!(*stage, BuildStage::Setup);
        assert_eq!(task.id, "setup-gh-models");
        assert_eq!(task.provider, "gh-models");
        assert_eq!(task.priority, 25);
    }

    #[test]
    fn test_contribute_runs_extension_install() {
        let contributor = GhModelsContributor;
        let ir = make_ir();
        let project = make_project_with_gh_models();

        let (contributions, _) = contributor.contribute(&ir, &project);
        let (_, task) = &contributions[0];

        assert_eq!(task.command.len(), 1);
        assert_eq!(task.command[0], "gh extension install github/gh-models");
    }

    #[test]
    fn test_contribute_is_idempotent() {
        let contributor = GhModelsContributor;
        let mut ir = make_ir();
        let project = make_project_with_gh_models();

        // First contribution should modify
        let (contributions, modified) = contributor.contribute(&ir, &project);
        assert!(modified);
        assert_eq!(contributions.len(), 1);

        // Add the task to IR
        for (stage, task) in contributions {
            ir.stages.add(stage, task);
        }

        // Second contribution should not modify
        let (contributions, modified) = contributor.contribute(&ir, &project);
        assert!(!modified);
        assert!(contributions.is_empty());
    }
}
