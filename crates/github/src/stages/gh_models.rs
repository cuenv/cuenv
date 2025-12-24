//! GitHub Models CLI Extension Stage Contributor
//!
//! Contributes the gh-models extension setup task to CI pipelines that use
//! `gh models` commands for LLM evaluation.
//!
//! Self-detects activation by checking if any task in the pipeline's IR uses
//! the `gh` command with `models` as the first argument.

use cuenv_ci::StageContributor;
use cuenv_ci::ir::{BuildStage, IntermediateRepresentation, StageTask, Task};
use cuenv_core::manifest::Project;
use std::collections::HashMap;

/// GitHub Models CLI extension stage contributor
///
/// Self-detects activation by checking if the pipeline's IR contains tasks that
/// use `gh models` commands (e.g., for LLM evaluation).
///
/// When active, contributes:
/// - Setup: Install the gh-models extension via `gh extension install github/gh-models`
#[derive(Debug, Clone, Copy, Default)]
pub struct GhModelsContributor;

impl GhModelsContributor {
    /// Check if an IR task uses `gh models` command
    fn ir_task_uses_gh_models(task: &Task) -> bool {
        // IR task command is Vec<String>, check if it's ["gh", "models", ...]
        // or a shell command containing "gh models"
        if task.command.len() >= 2 {
            task.command[0] == "gh" && task.command[1] == "models"
        } else if task.command.len() == 1 && task.shell {
            // Shell command - check if it contains "gh models"
            task.command[0].contains("gh models")
        } else {
            false
        }
    }

    /// Check if any of the pipeline's tasks use `gh models`
    fn pipeline_uses_gh_models(ir: &IntermediateRepresentation) -> bool {
        // If no specific pipeline tasks are set, check all IR tasks
        if ir.pipeline.pipeline_tasks.is_empty() {
            return ir.tasks.iter().any(Self::ir_task_uses_gh_models);
        }

        // Only check tasks that are part of this pipeline
        ir.tasks
            .iter()
            .filter(|task| ir.pipeline.pipeline_tasks.contains(&task.id))
            .any(Self::ir_task_uses_gh_models)
    }
}

impl StageContributor for GhModelsContributor {
    fn id(&self) -> &'static str {
        "gh-models"
    }

    fn is_active(&self, ir: &IntermediateRepresentation, _project: &Project) -> bool {
        // Self-detect: check if this pipeline's tasks use gh models
        Self::pipeline_uses_gh_models(ir)
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
    use cuenv_ci::ir::{CachePolicy, PipelineMetadata, StageConfiguration};

    fn make_ir() -> IntermediateRepresentation {
        IntermediateRepresentation {
            version: "1.4".to_string(),
            pipeline: PipelineMetadata {
                name: "test".to_string(),
                environment: None,
                requires_onepassword: false,
                project_name: None,
                trigger: None,
                pipeline_tasks: vec![],
            },
            runtimes: vec![],
            stages: StageConfiguration::default(),
            tasks: vec![],
        }
    }

    fn make_ir_task(id: &str, command: Vec<&str>) -> Task {
        Task {
            id: id.to_string(),
            runtime: None,
            command: command.into_iter().map(String::from).collect(),
            shell: false,
            env: HashMap::new(),
            secrets: HashMap::new(),
            resources: None,
            concurrency_group: None,
            inputs: vec![],
            outputs: vec![],
            depends_on: vec![],
            cache_policy: CachePolicy::Normal,
            deployment: false,
            manual_approval: false,
            matrix: None,
            artifact_downloads: vec![],
            params: HashMap::new(),
        }
    }

    fn make_project() -> Project {
        Project::new("test")
    }

    #[test]
    fn test_is_active_with_gh_models_task() {
        let contributor = GhModelsContributor;
        let mut ir = make_ir();
        ir.tasks.push(make_ir_task(
            "eval.task-gen",
            vec!["gh", "models", "eval", "prompts/test.yml"],
        ));
        let project = make_project();

        assert!(contributor.is_active(&ir, &project));
    }

    #[test]
    fn test_is_inactive_without_gh_models_task() {
        let contributor = GhModelsContributor;
        let mut ir = make_ir();
        ir.tasks.push(make_ir_task("build", vec!["cargo", "build"]));
        let project = make_project();

        assert!(!contributor.is_active(&ir, &project));
    }

    #[test]
    fn test_is_inactive_with_empty_ir() {
        let contributor = GhModelsContributor;
        let ir = make_ir();
        let project = make_project();

        assert!(!contributor.is_active(&ir, &project));
    }

    #[test]
    fn test_contribute_returns_setup_task() {
        let contributor = GhModelsContributor;
        let mut ir = make_ir();
        ir.tasks.push(make_ir_task(
            "eval.task-gen",
            vec!["gh", "models", "eval", "prompts/test.yml"],
        ));
        let project = make_project();

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
        let project = make_project();

        let (contributions, _) = contributor.contribute(&ir, &project);
        let (_, task) = &contributions[0];

        assert_eq!(task.command.len(), 1);
        assert_eq!(task.command[0], "gh extension install github/gh-models");
    }

    #[test]
    fn test_contribute_is_idempotent() {
        let contributor = GhModelsContributor;
        let mut ir = make_ir();
        ir.tasks.push(make_ir_task(
            "eval.task-gen",
            vec!["gh", "models", "eval", "prompts/test.yml"],
        ));
        let project = make_project();

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

    #[test]
    fn test_is_inactive_when_gh_models_not_in_pipeline_tasks() {
        let contributor = GhModelsContributor;
        let mut ir = make_ir();
        // Add a gh models task to IR
        ir.tasks.push(make_ir_task(
            "eval.task-gen",
            vec!["gh", "models", "eval", "prompts/test.yml"],
        ));
        // Add a build task
        ir.tasks.push(make_ir_task("build", vec!["cargo", "build"]));
        // But pipeline only runs the build task
        ir.pipeline.pipeline_tasks = vec!["build".to_string()];
        let project = make_project();

        // Should be inactive because gh models task is not in pipeline_tasks
        assert!(!contributor.is_active(&ir, &project));
    }

    #[test]
    fn test_is_active_when_gh_models_in_pipeline_tasks() {
        let contributor = GhModelsContributor;
        let mut ir = make_ir();
        // Add tasks
        ir.tasks.push(make_ir_task(
            "eval.task-gen",
            vec!["gh", "models", "eval", "prompts/test.yml"],
        ));
        ir.tasks.push(make_ir_task("build", vec!["cargo", "build"]));
        // Pipeline runs both tasks
        ir.pipeline.pipeline_tasks = vec!["eval.task-gen".to_string(), "build".to_string()];
        let project = make_project();

        // Should be active because gh models task is in pipeline_tasks
        assert!(contributor.is_active(&ir, &project));
    }
}
