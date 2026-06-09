use super::Compiler;
use crate::ir::{
    BuildStage, CachePolicy, IntermediateRepresentation, SecretConfig, Task as IrTask,
    TaskCondition,
};
use cuenv_core::ci::{Contributor, ContributorTask, SecretRef, TaskCondition as CueTaskCondition};
use std::collections::{BTreeMap, HashSet};

#[derive(Debug, Clone, PartialEq, Eq)]
struct CachixSettings {
    name: String,
    auth_token_secret: String,
}

impl Compiler {
    /// Apply CUE-defined contributors to the IR.
    pub(super) fn apply_cue_contributors(&self, ir: &mut IntermediateRepresentation) {
        let Some(ref ci_config) = self.project.ci else {
            return;
        };

        for contributor in &ci_config.contributors {
            if !self.cue_contributor_is_active(contributor, ir) {
                continue;
            }

            let contributed_ids: HashSet<String> = ir
                .tasks
                .iter()
                .filter(|t| t.phase.is_some())
                .map(|t| t.id.clone())
                .collect();

            for contributor_task in &contributor.tasks {
                let full_task_id = format!("cuenv:contributor:{}", contributor_task.id);

                if contributed_ids.contains(&full_task_id) {
                    continue;
                }

                let task = self.contributor_task_to_ir(contributor_task, &contributor.id);
                ir.tasks.push(task);
            }
        }
    }

    /// Check if a CUE contributor is active.
    pub(super) fn cue_contributor_is_active(
        &self,
        contributor: &Contributor,
        ir: &IntermediateRepresentation,
    ) -> bool {
        let Some(ref condition) = contributor.when else {
            return true;
        };

        if let Some(always_val) = condition.always {
            return always_val;
        }

        if !condition.runtime_type.is_empty() {
            if let Some(ref runtime) = self.project.runtime {
                let runtime_type = Self::get_runtime_type(runtime);
                if !condition.runtime_type.iter().any(|t| t == runtime_type) {
                    return false;
                }
            } else {
                return false;
            }
        }

        if !condition.cuenv_source.is_empty() {
            let source = self
                .project
                .config
                .as_ref()
                .and_then(|c| c.ci.as_ref())
                .and_then(|ci| ci.cuenv.as_ref())
                .map_or("release", |c| c.source.as_str());
            if !condition.cuenv_source.iter().any(|s| s == source) {
                return false;
            }
        }

        if !condition.secrets_provider.is_empty()
            && !self.has_secrets_provider(&condition.secrets_provider, ir)
        {
            return false;
        }

        if !condition.provider_config.is_empty()
            && !self.has_provider_config(&condition.provider_config)
        {
            return false;
        }

        if !condition.task_command.is_empty()
            && !Self::has_task_command(&condition.task_command, ir)
        {
            return false;
        }

        if !condition.task_labels.is_empty() && !self.has_task_labels(&condition.task_labels) {
            return false;
        }

        if !condition.environment.is_empty() {
            let Some(ref pipeline) = self.options.pipeline else {
                return false;
            };
            let Some(ref env_name) = pipeline.environment else {
                return false;
            };
            if !condition.environment.iter().any(|e| e == env_name) {
                return false;
            }
        }

        if !condition.workspace_member.is_empty() {
            let module_root = self
                .options
                .module_root
                .clone()
                .or_else(|| self.options.project_root.clone())
                .unwrap_or_else(|| std::path::PathBuf::from("."));

            let detected = self.detect_workspace_managers(&module_root);

            if !condition
                .workspace_member
                .iter()
                .any(|t| detected.contains(&t.to_lowercase()))
            {
                return false;
            }
        }

        true
    }

    /// Detect package managers for the workspace, checking if the current project is a member.
    fn detect_workspace_managers(&self, module_root: &std::path::Path) -> Vec<String> {
        use cuenv_workspaces::{PackageJsonDiscovery, WorkspaceDiscovery};

        if let Ok(workspace) = PackageJsonDiscovery.discover(module_root) {
            if let Some(ref project_path) = self.options.project_path {
                let path = std::path::Path::new(project_path);
                if workspace.contains_path(path) || workspace.lockfile.is_some() {
                    return vec![workspace.manager.to_string().to_lowercase()];
                }
            } else {
                return vec![workspace.manager.to_string().to_lowercase()];
            }
        }

        cuenv_workspaces::detection::detect_package_managers(module_root)
            .unwrap_or_default()
            .into_iter()
            .map(|m| m.to_string().to_lowercase())
            .collect()
    }

    /// Get the runtime type string for condition matching.
    fn get_runtime_type(runtime: &cuenv_core::manifest::Runtime) -> &'static str {
        match runtime {
            cuenv_core::manifest::Runtime::Nix(_) => "nix",
            cuenv_core::manifest::Runtime::Devenv(_) => "devenv",
            cuenv_core::manifest::Runtime::Container(_) => "container",
            cuenv_core::manifest::Runtime::Dagger(_) => "dagger",
            cuenv_core::manifest::Runtime::Oci(_) => "oci",
            cuenv_core::manifest::Runtime::Tools(_) => "tools",
        }
    }

    /// Check if the pipeline environment uses any of the specified secrets providers.
    fn has_secrets_provider(&self, providers: &[String], ir: &IntermediateRepresentation) -> bool {
        let Some(ref env_name) = ir.pipeline.environment else {
            return false;
        };
        let Some(ref env) = self.project.env else {
            return false;
        };

        let env_vars = env.for_environment(env_name);
        for value in env_vars.values() {
            if Self::value_has_provider(value, providers) {
                return true;
            }
        }
        false
    }

    /// Check if an `EnvValue` uses any of the specified secret providers.
    pub(super) fn value_has_provider(
        value: &cuenv_core::environment::EnvValue,
        providers: &[String],
    ) -> bool {
        use cuenv_core::environment::{EnvValue, EnvValueSimple};

        match value {
            EnvValue::String(s)
                if providers.iter().any(|p| p == "onepassword") && s.starts_with("op://") =>
            {
                true
            }
            EnvValue::Secret(secret) => providers.iter().any(|p| p == &secret.resolver),
            EnvValue::Interpolated(parts) => Self::parts_have_provider(parts, providers),
            EnvValue::WithPolicies(wp) => match &wp.value {
                EnvValueSimple::Secret(secret) => providers.iter().any(|p| p == &secret.resolver),
                EnvValueSimple::String(s)
                    if providers.iter().any(|p| p == "onepassword") && s.starts_with("op://") =>
                {
                    true
                }
                EnvValueSimple::Interpolated(parts) => Self::parts_have_provider(parts, providers),
                _ => false,
            },
            _ => false,
        }
    }

    /// Check if any part in an interpolated array uses one of the specified providers.
    pub(super) fn parts_have_provider(
        parts: &[cuenv_core::environment::EnvPart],
        providers: &[String],
    ) -> bool {
        use cuenv_core::environment::EnvPart;

        parts.iter().any(|part| match part {
            EnvPart::Secret(secret) => providers.iter().any(|p| p == &secret.resolver),
            EnvPart::Literal(s) => {
                providers.iter().any(|p| p == "onepassword") && s.contains("op://")
            }
        })
    }

    /// Check if any of the specified provider config paths are set.
    fn has_provider_config(&self, paths: &[String]) -> bool {
        let Some(ref ci) = self.project.ci else {
            return false;
        };
        let Some(ref provider) = ci.provider else {
            return false;
        };

        for path in paths {
            let parts: Vec<&str> = path.split('.').collect();
            if parts.is_empty() {
                continue;
            }

            let Some(config) = provider.get(parts[0]) else {
                continue;
            };

            let mut current = config;
            let mut found = true;
            for part in &parts[1..] {
                match current.get(*part) {
                    Some(value) if !value.is_null() => {
                        current = value;
                    }
                    _ => {
                        found = false;
                        break;
                    }
                }
            }

            if found {
                return true;
            }
        }

        false
    }

    /// Check if any pipeline task uses the specified command.
    fn has_task_command(commands: &[String], ir: &IntermediateRepresentation) -> bool {
        for task in &ir.tasks {
            if !ir.pipeline.pipeline_tasks.is_empty()
                && !ir.pipeline.pipeline_tasks.contains(&task.id)
            {
                continue;
            }

            if task.command.len() >= commands.len() {
                let matches = commands
                    .iter()
                    .zip(task.command.iter())
                    .all(|(a, b)| a == b);
                if matches {
                    return true;
                }
            }

            if task.shell && task.command.len() == 1 {
                let cmd_str = commands.join(" ");
                if task.command[0].contains(&cmd_str) {
                    return true;
                }
            }
        }

        false
    }

    /// Check if any pipeline task has the specified labels.
    fn has_task_labels(&self, labels: &[String]) -> bool {
        let Some(ref pipeline) = self.options.pipeline else {
            return false;
        };

        for pipeline_task in &pipeline.tasks {
            let task_name = pipeline_task.task_name();
            if let Some(task) = self.find_task(task_name) {
                let has_all = labels.iter().all(|l| task.labels.contains(l));
                if has_all {
                    return true;
                }
            }
        }

        false
    }

    /// Derive the build stage from contributor task priority.
    pub(super) fn derive_stage_from_priority(
        priority: i32,
        condition: Option<CueTaskCondition>,
    ) -> BuildStage {
        if matches!(condition, Some(CueTaskCondition::OnFailure)) {
            return BuildStage::Failure;
        }

        match priority {
            0..=9 => BuildStage::Bootstrap,
            10..=49 => BuildStage::Setup,
            _ => BuildStage::Success,
        }
    }

    /// Convert a CUE TaskCondition to an IR TaskCondition.
    pub(super) fn cue_task_condition_to_ir(condition: CueTaskCondition) -> TaskCondition {
        match condition {
            CueTaskCondition::OnSuccess => TaskCondition::OnSuccess,
            CueTaskCondition::OnFailure => TaskCondition::OnFailure,
            CueTaskCondition::Always => TaskCondition::Always,
        }
    }

    /// Convert a CUE ContributorTask to an IR Task.
    pub(super) fn contributor_task_to_ir(
        &self,
        contributor_task: &ContributorTask,
        contributor_id: &str,
    ) -> IrTask {
        let resolve_value = |value: &str| self.resolve_contributor_value(contributor_id, value);

        let (command, shell) = if let Some(ref cmd) = contributor_task.command {
            let mut cmd_vec = vec![resolve_value(cmd)];
            cmd_vec.extend(contributor_task.args.iter().map(|arg| resolve_value(arg)));

            let has_github_action = contributor_task
                .provider
                .as_ref()
                .is_some_and(|p| p.github.is_some());
            let is_cuenv_contributor = contributor_id == "cuenv";
            let is_bootstrap = contributor_task.priority < 10;
            let runs_before_cuenv = is_cuenv_contributor || is_bootstrap;
            let needs_wrapping = !has_github_action && cmd != "cuenv" && !runs_before_cuenv;

            if needs_wrapping {
                let mut wrapped = vec!["cuenv".to_string(), "exec".to_string(), "--".to_string()];
                wrapped.extend(cmd_vec);
                (wrapped, contributor_task.shell)
            } else {
                (cmd_vec, contributor_task.shell)
            }
        } else if let Some(ref script) = contributor_task.script {
            (vec![resolve_value(script)], true)
        } else {
            (vec![], false)
        };

        let secrets: BTreeMap<String, SecretConfig> = contributor_task
            .secrets
            .iter()
            .map(|(k, v)| {
                let config = match v {
                    SecretRef::Simple(s) => SecretConfig {
                        source: s.clone(),
                        cache_key: false,
                    },
                    SecretRef::Detailed(d) => SecretConfig {
                        source: d.source.clone(),
                        cache_key: d.cache_key,
                    },
                };
                (k.clone(), config)
            })
            .collect();

        let provider_hints = contributor_task.provider.as_ref().and_then(|p| {
            p.github.as_ref().map(|gh| {
                let mut github_action = serde_json::Map::new();
                github_action.insert(
                    "uses".to_string(),
                    serde_json::Value::String(gh.uses.clone()),
                );
                if !gh.inputs.is_empty() {
                    github_action.insert(
                        "inputs".to_string(),
                        serde_json::Value::Object(
                            gh.inputs
                                .iter()
                                .map(|(k, v)| {
                                    let value = match v {
                                        serde_json::Value::String(s) => {
                                            serde_json::Value::String(resolve_value(s))
                                        }
                                        _ => v.clone(),
                                    };
                                    (k.clone(), value)
                                })
                                .collect(),
                        ),
                    );
                }
                if let Some(condition) = &gh.if_condition {
                    github_action.insert(
                        "if".to_string(),
                        serde_json::Value::String(resolve_value(condition)),
                    );
                }

                let mut hints = serde_json::Map::new();
                hints.insert(
                    "github_action".to_string(),
                    serde_json::Value::Object(github_action),
                );
                serde_json::Value::Object(hints)
            })
        });

        let condition = contributor_task
            .condition
            .map(Self::cue_task_condition_to_ir);
        let stage =
            Self::derive_stage_from_priority(contributor_task.priority, contributor_task.condition);

        let depends_on: Vec<String> = contributor_task
            .depends_on
            .iter()
            .map(|dep| {
                if dep.starts_with("cuenv:contributor:") {
                    dep.clone()
                } else {
                    format!("cuenv:contributor:{dep}")
                }
            })
            .collect();

        IrTask {
            id: format!("cuenv:contributor:{}", contributor_task.id),
            runtime: None,
            command,
            shell,
            env: contributor_task
                .env
                .iter()
                .map(|(k, v)| (k.clone(), resolve_value(v)))
                .collect(),
            secrets,
            resources: None,
            concurrency_group: None,
            inputs: contributor_task.inputs.clone(),
            outputs: vec![],
            depends_on,
            cache_policy: CachePolicy::Disabled,
            deployment: false,
            manual_approval: false,
            matrix: None,
            artifact_downloads: vec![],
            params: BTreeMap::new(),
            phase: Some(stage),
            label: contributor_task.label.clone(),
            priority: Some(contributor_task.priority),
            contributor: Some(contributor_id.to_string()),
            condition,
            provider_hints,
        }
    }

    fn resolve_contributor_value(&self, contributor_id: &str, value: &str) -> String {
        match contributor_id {
            "cachix" => self.resolve_cachix_value(value),
            "cuenv" => self.resolve_cuenv_value(value),
            _ => value.to_string(),
        }
    }

    fn resolve_cuenv_value(&self, value: &str) -> String {
        value.replace("${CUENV_VERSION}", self.configured_cuenv_version())
    }

    fn configured_cuenv_version(&self) -> &str {
        let configured = self
            .project
            .config
            .as_ref()
            .and_then(|config| config.ci.as_ref())
            .and_then(|ci| ci.cuenv.as_ref())
            .map(|cuenv| cuenv.version.as_str());

        match configured {
            Some("self") | None => cuenv_core::VERSION,
            Some(version) => version,
        }
    }

    fn resolve_cachix_value(&self, value: &str) -> String {
        let Some(settings) = self.cachix_settings() else {
            return value.to_string();
        };

        if value == "${CACHIX_AUTH_TOKEN}" {
            return format!("${{{}}}", settings.auth_token_secret);
        }

        value.replace("${CACHIX_CACHE_NAME}", &settings.name)
    }

    fn cachix_settings(&self) -> Option<CachixSettings> {
        let mut config = self
            .project
            .ci
            .as_ref()
            .and_then(|ci| ci.provider.as_ref())
            .and_then(|provider| provider.get("github"))
            .and_then(Self::parse_cachix_settings);

        if let Some(pipeline_config) = self
            .options
            .pipeline
            .as_ref()
            .and_then(|pipeline| pipeline.provider.as_ref())
            .and_then(|provider| provider.get("github"))
            .and_then(Self::parse_cachix_settings)
        {
            config = Some(match config {
                Some(global) => CachixSettings {
                    name: pipeline_config.name,
                    auth_token_secret: if pipeline_config.auth_token_secret.is_empty() {
                        global.auth_token_secret
                    } else {
                        pipeline_config.auth_token_secret
                    },
                },
                None => pipeline_config,
            });
        }

        config
    }

    fn parse_cachix_settings(value: &serde_json::Value) -> Option<CachixSettings> {
        let github = value.as_object()?;
        let cachix = github.get("cachix")?.as_object()?;
        let name = cachix.get("name")?.as_str()?.to_string();
        let auth_token_secret = cachix
            .get("authToken")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("CACHIX_AUTH_TOKEN")
            .to_string();

        Some(CachixSettings {
            name,
            auth_token_secret,
        })
    }
}
