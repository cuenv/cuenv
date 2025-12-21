//! 1Password Stage Contributor
//!
//! Contributes 1Password WASM SDK setup task to the CI pipeline.

use super::StageContributor;
use crate::ir::{BuildStage, IntermediateRepresentation, StageTask};
use cuenv_core::manifest::Project;
use std::collections::HashMap;

/// 1Password stage contributor
///
/// When active (`pipeline.requires_onepassword` is true), contributes:
/// - Setup: Run `cuenv secrets setup onepassword` to initialize the WASM SDK
#[derive(Debug, Clone, Copy, Default)]
pub struct OnePasswordContributor;

impl StageContributor for OnePasswordContributor {
    fn id(&self) -> &'static str {
        "1password"
    }

    fn is_active(&self, ir: &IntermediateRepresentation, _project: &Project) -> bool {
        ir.pipeline.requires_onepassword
    }

    fn contribute(
        &self,
        _ir: &IntermediateRepresentation,
        _project: &Project,
    ) -> Vec<(BuildStage, StageTask)> {
        let mut env = HashMap::new();
        env.insert(
            "OP_SERVICE_ACCOUNT_TOKEN".to_string(),
            "${OP_SERVICE_ACCOUNT_TOKEN}".to_string(),
        );

        vec![(
            BuildStage::Setup,
            StageTask {
                id: "setup-1password".to_string(),
                provider: "1password".to_string(),
                label: Some("Setup 1Password".to_string()),
                command: vec!["cuenv secrets setup onepassword".to_string()],
                shell: false,
                env,
                // Depends on cuenv being installed/built
                depends_on: vec!["setup-cuenv".to_string()],
                priority: 20,
                ..Default::default()
            },
        )]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{PipelineMetadata, StageConfiguration};

    fn make_ir_with_onepassword() -> IntermediateRepresentation {
        IntermediateRepresentation {
            version: "1.4".to_string(),
            pipeline: PipelineMetadata {
                name: "test".to_string(),
                environment: None,
                requires_onepassword: true,
                project_name: None,
                trigger: None,
            },
            runtimes: vec![],
            stages: StageConfiguration::default(),
            tasks: vec![],
        }
    }

    fn make_ir_without_onepassword() -> IntermediateRepresentation {
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

    fn make_project() -> Project {
        Project {
            name: "test".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn test_is_active_with_onepassword() {
        let contributor = OnePasswordContributor;
        let ir = make_ir_with_onepassword();
        let project = make_project();

        assert!(contributor.is_active(&ir, &project));
    }

    #[test]
    fn test_is_active_without_onepassword() {
        let contributor = OnePasswordContributor;
        let ir = make_ir_without_onepassword();
        let project = make_project();

        assert!(!contributor.is_active(&ir, &project));
    }

    #[test]
    fn test_contribute_returns_setup_task() {
        let contributor = OnePasswordContributor;
        let ir = make_ir_with_onepassword();
        let project = make_project();

        let contributions = contributor.contribute(&ir, &project);

        assert_eq!(contributions.len(), 1);

        let (stage, task) = &contributions[0];
        assert_eq!(*stage, BuildStage::Setup);
        assert_eq!(task.id, "setup-1password");
        assert_eq!(task.provider, "1password");
        assert_eq!(task.priority, 20);
    }

    #[test]
    fn test_contribute_sets_env_var() {
        let contributor = OnePasswordContributor;
        let ir = make_ir_with_onepassword();
        let project = make_project();

        let contributions = contributor.contribute(&ir, &project);
        let (_, task) = &contributions[0];

        assert!(task.env.contains_key("OP_SERVICE_ACCOUNT_TOKEN"));
        assert_eq!(
            task.env.get("OP_SERVICE_ACCOUNT_TOKEN").unwrap(),
            "${OP_SERVICE_ACCOUNT_TOKEN}"
        );
    }

    #[test]
    fn test_contribute_runs_setup_command() {
        let contributor = OnePasswordContributor;
        let ir = make_ir_with_onepassword();
        let project = make_project();

        let contributions = contributor.contribute(&ir, &project);
        let (_, task) = &contributions[0];

        assert_eq!(task.command.len(), 1);
        assert_eq!(task.command[0], "cuenv secrets setup onepassword");
    }

    #[test]
    fn test_contribute_depends_on_setup_cuenv() {
        let contributor = OnePasswordContributor;
        let ir = make_ir_with_onepassword();
        let project = make_project();

        let contributions = contributor.contribute(&ir, &project);
        let (_, task) = &contributions[0];

        assert!(task.depends_on.contains(&"setup-cuenv".to_string()));
    }
}
