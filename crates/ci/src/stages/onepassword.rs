//! 1Password Stage Contributor
//!
//! Contributes 1Password WASM SDK setup task to the CI pipeline.
//!
//! This contributor self-detects when 1Password is needed by examining
//! the pipeline's environment for `#OnePasswordRef` secrets.

use super::StageContributor;
use crate::ir::{BuildStage, IntermediateRepresentation, StageTask};
use cuenv_core::environment::{Env, EnvValue, EnvValueSimple};
use cuenv_core::manifest::Project;
use std::collections::HashMap;

/// 1Password stage contributor
///
/// Self-detects activation by checking if the pipeline's environment
/// contains 1Password secret references (resolver="onepassword" or "op://" URIs).
///
/// When active, contributes:
/// - Setup: Run `cuenv secrets setup onepassword` to initialize the WASM SDK
#[derive(Debug, Clone, Copy, Default)]
pub struct OnePasswordContributor;

impl OnePasswordContributor {
    /// Check if an environment contains 1Password secret references
    fn environment_has_onepassword_refs(env: &Env, environment_name: &str) -> bool {
        let env_vars = env.for_environment(environment_name);
        env_vars.values().any(|value| match value {
            EnvValue::String(s) => s.starts_with("op://"),
            EnvValue::Secret(secret) => secret.resolver == "onepassword",
            EnvValue::WithPolicies(with_policies) => match &with_policies.value {
                EnvValueSimple::Secret(secret) => secret.resolver == "onepassword",
                EnvValueSimple::String(s) => s.starts_with("op://"),
                _ => false,
            },
            _ => false,
        })
    }
}

impl StageContributor for OnePasswordContributor {
    fn id(&self) -> &'static str {
        "1password"
    }

    fn is_active(&self, ir: &IntermediateRepresentation, project: &Project) -> bool {
        // Self-detect: check if the pipeline's environment has 1Password refs
        let Some(env_name) = &ir.pipeline.environment else {
            return false;
        };
        let Some(env) = &project.env else {
            return false;
        };
        Self::environment_has_onepassword_refs(env, env_name)
    }

    fn contribute(
        &self,
        ir: &IntermediateRepresentation,
        _project: &Project,
    ) -> (Vec<(BuildStage, StageTask)>, bool) {
        // Idempotency: check if already contributed
        if ir.stages.setup.iter().any(|t| t.id == "setup-1password") {
            return (vec![], false);
        }

        let mut env = HashMap::new();
        env.insert(
            "OP_SERVICE_ACCOUNT_TOKEN".to_string(),
            "${OP_SERVICE_ACCOUNT_TOKEN}".to_string(),
        );

        (
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
            )],
            true,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{PipelineMetadata, StageConfiguration};
    use cuenv_core::secrets::Secret;

    /// Create IR with a production environment set
    fn make_ir_with_production_env() -> IntermediateRepresentation {
        IntermediateRepresentation {
            version: "1.4".to_string(),
            pipeline: PipelineMetadata {
                name: "test".to_string(),
                environment: Some("production".to_string()),
                requires_onepassword: false, // No longer used for detection
                project_name: None,
                trigger: None,
                pipeline_tasks: vec![],
            },
            runtimes: vec![],
            stages: StageConfiguration::default(),
            tasks: vec![],
        }
    }

    /// Create IR without any environment set
    fn make_ir_without_env() -> IntermediateRepresentation {
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

    /// Create project with 1Password secrets in production environment
    fn make_project_with_onepassword_env() -> Project {
        let mut env_overrides = HashMap::new();
        let mut production_env = HashMap::new();
        production_env.insert(
            "CLOUDFLARE_API_TOKEN".to_string(),
            EnvValue::Secret(Secret::onepassword("op://vault/item/field")),
        );
        env_overrides.insert("production".to_string(), production_env);

        Project {
            name: "test".to_string(),
            env: Some(Env {
                base: HashMap::new(),
                environment: Some(env_overrides),
            }),
            ..Default::default()
        }
    }

    /// Create project with op:// URI string in production environment
    fn make_project_with_op_uri() -> Project {
        let mut env_overrides = HashMap::new();
        let mut production_env = HashMap::new();
        production_env.insert(
            "API_TOKEN".to_string(),
            EnvValue::String("op://vault/item/field".to_string()),
        );
        env_overrides.insert("production".to_string(), production_env);

        Project {
            name: "test".to_string(),
            env: Some(Env {
                base: HashMap::new(),
                environment: Some(env_overrides),
            }),
            ..Default::default()
        }
    }

    /// Create project without 1Password secrets
    fn make_project_without_onepassword() -> Project {
        let mut env_overrides = HashMap::new();
        let mut production_env = HashMap::new();
        production_env.insert(
            "SOME_VAR".to_string(),
            EnvValue::String("value".to_string()),
        );
        env_overrides.insert("production".to_string(), production_env);

        Project {
            name: "test".to_string(),
            env: Some(Env {
                base: HashMap::new(),
                environment: Some(env_overrides),
            }),
            ..Default::default()
        }
    }

    /// Create project with no env configuration
    fn make_project_without_env() -> Project {
        Project {
            name: "test".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn test_is_active_with_onepassword_secret() {
        let contributor = OnePasswordContributor;
        let ir = make_ir_with_production_env();
        let project = make_project_with_onepassword_env();

        assert!(contributor.is_active(&ir, &project));
    }

    #[test]
    fn test_is_active_with_op_uri() {
        let contributor = OnePasswordContributor;
        let ir = make_ir_with_production_env();
        let project = make_project_with_op_uri();

        assert!(contributor.is_active(&ir, &project));
    }

    #[test]
    fn test_is_inactive_without_onepassword() {
        let contributor = OnePasswordContributor;
        let ir = make_ir_with_production_env();
        let project = make_project_without_onepassword();

        assert!(!contributor.is_active(&ir, &project));
    }

    #[test]
    fn test_is_inactive_without_environment() {
        let contributor = OnePasswordContributor;
        let ir = make_ir_without_env();
        let project = make_project_with_onepassword_env();

        // No environment set on IR, so contributor is inactive
        assert!(!contributor.is_active(&ir, &project));
    }

    #[test]
    fn test_is_inactive_without_project_env() {
        let contributor = OnePasswordContributor;
        let ir = make_ir_with_production_env();
        let project = make_project_without_env();

        // Project has no env config, so contributor is inactive
        assert!(!contributor.is_active(&ir, &project));
    }

    #[test]
    fn test_contribute_returns_setup_task() {
        let contributor = OnePasswordContributor;
        let ir = make_ir_with_production_env();
        let project = make_project_with_onepassword_env();

        let (contributions, modified) = contributor.contribute(&ir, &project);

        assert!(modified);
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
        let ir = make_ir_with_production_env();
        let project = make_project_with_onepassword_env();

        let (contributions, _) = contributor.contribute(&ir, &project);
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
        let ir = make_ir_with_production_env();
        let project = make_project_with_onepassword_env();

        let (contributions, _) = contributor.contribute(&ir, &project);
        let (_, task) = &contributions[0];

        assert_eq!(task.command.len(), 1);
        assert_eq!(task.command[0], "cuenv secrets setup onepassword");
    }

    #[test]
    fn test_contribute_depends_on_setup_cuenv() {
        let contributor = OnePasswordContributor;
        let ir = make_ir_with_production_env();
        let project = make_project_with_onepassword_env();

        let (contributions, _) = contributor.contribute(&ir, &project);
        let (_, task) = &contributions[0];

        assert!(task.depends_on.contains(&"setup-cuenv".to_string()));
    }

    #[test]
    fn test_contribute_is_idempotent() {
        let contributor = OnePasswordContributor;
        let mut ir = make_ir_with_production_env();
        let project = make_project_with_onepassword_env();

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
