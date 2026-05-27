use super::*;

#[test]
fn test_contributor_no_condition_always_active() {
    let project = Project::new("test");
    let compiler = Compiler::new(project);
    let ir = test_ir();

    // No `when` condition = always active
    let contributor = test_contributor("test", None);
    assert!(compiler.cue_contributor_is_active(&contributor, &ir));
}

#[test]
fn test_contributor_always_true_active() {
    let project = Project::new("test");
    let compiler = Compiler::new(project);
    let ir = test_ir();

    let contributor = test_contributor(
        "test",
        Some(ActivationCondition {
            always: Some(true),
            ..Default::default()
        }),
    );
    assert!(compiler.cue_contributor_is_active(&contributor, &ir));
}

#[test]
fn test_contributor_always_false_inactive() {
    let project = Project::new("test");
    let compiler = Compiler::new(project);
    let ir = test_ir();

    // always: false explicitly disables the contributor
    let contributor = test_contributor(
        "test",
        Some(ActivationCondition {
            always: Some(false),
            ..Default::default()
        }),
    );
    assert!(!compiler.cue_contributor_is_active(&contributor, &ir));
}

#[test]
fn test_contributor_runtime_type_matches_nix() {
    use cuenv_core::manifest::{NixRuntime, Runtime};

    let mut project = Project::new("test");
    project.runtime = Some(Runtime::Nix(NixRuntime::default()));

    let compiler = Compiler::new(project);
    let ir = test_ir();

    let contributor = test_contributor(
        "nix",
        Some(ActivationCondition {
            runtime_type: vec!["nix".to_string()],
            ..Default::default()
        }),
    );
    assert!(compiler.cue_contributor_is_active(&contributor, &ir));
}

#[test]
fn test_contributor_runtime_type_no_match() {
    use cuenv_core::manifest::{NixRuntime, Runtime};

    let mut project = Project::new("test");
    project.runtime = Some(Runtime::Nix(NixRuntime::default()));

    let compiler = Compiler::new(project);
    let ir = test_ir();

    // Project has Nix runtime, but condition requires "devenv"
    let contributor = test_contributor(
        "devenv-only",
        Some(ActivationCondition {
            runtime_type: vec!["devenv".to_string()],
            ..Default::default()
        }),
    );
    assert!(!compiler.cue_contributor_is_active(&contributor, &ir));
}

#[test]
fn test_contributor_runtime_type_no_runtime_set() {
    let project = Project::new("test");
    let compiler = Compiler::new(project);
    let ir = test_ir();

    // No runtime set but condition requires runtime type
    let contributor = test_contributor(
        "needs-nix",
        Some(ActivationCondition {
            runtime_type: vec!["nix".to_string()],
            ..Default::default()
        }),
    );
    assert!(!compiler.cue_contributor_is_active(&contributor, &ir));
}

#[test]
fn test_contributor_cuenv_source_matches() {
    use cuenv_core::ci::CI;
    use cuenv_core::config::{CIConfig, CuenvConfig, CuenvSource};
    use std::collections::BTreeMap;

    let mut project = Project::new("test");
    project.config = Some(cuenv_core::config::Config::default());
    project.ci = Some(CI {
        pipelines: BTreeMap::new(),
        ..Default::default()
    });
    // Set cuenv source to "git"
    if let Some(ref mut config) = project.config {
        config.ci = Some(CIConfig {
            cuenv: Some(CuenvConfig {
                source: CuenvSource::Git,
                ..Default::default()
            }),
        });
    }

    let compiler = Compiler::new(project);
    let ir = test_ir();

    let contributor = test_contributor(
        "cuenv-git",
        Some(ActivationCondition {
            cuenv_source: vec!["git".to_string()],
            ..Default::default()
        }),
    );
    assert!(compiler.cue_contributor_is_active(&contributor, &ir));
}

#[test]
fn test_contributor_multiple_conditions_and_logic() {
    use cuenv_core::manifest::{NixRuntime, Runtime};

    let mut project = Project::new("test");
    project.runtime = Some(Runtime::Nix(NixRuntime::default()));

    let compiler = Compiler::new(project);
    let ir = test_ir();

    // Condition requires nix runtime AND devenv source (which doesn't match)
    let contributor = test_contributor(
        "multi-condition",
        Some(ActivationCondition {
            runtime_type: vec!["nix".to_string()],
            cuenv_source: vec!["nix".to_string()], // default is "release", not "nix"
            ..Default::default()
        }),
    );
    // Runtime matches but cuenv_source doesn't (default is "release")
    assert!(!compiler.cue_contributor_is_active(&contributor, &ir));
}

// =========================================================================
// Contributor Task Conversion Tests
// =========================================================================

#[test]
fn test_contributor_task_to_ir_command() {
    let contributor_task = ContributorTask {
        id: "test-task".to_string(),
        label: Some("Test Task".to_string()),
        description: None,
        command: Some("echo".to_string()),
        args: vec!["hello".to_string()],
        script: None,
        shell: false,
        env: HashMap::default(),
        secrets: HashMap::default(),
        inputs: vec![],
        outputs: vec![],
        hermetic: false,
        depends_on: vec![],
        priority: 10,
        condition: None,
        provider: None,
    };

    let ir_task = test_compiler().contributor_task_to_ir(&contributor_task, "github");

    assert_eq!(ir_task.id, "cuenv:contributor:test-task");
    // Commands are wrapped with cuenv exec for tool activation
    assert_eq!(
        ir_task.command,
        vec!["cuenv", "exec", "--", "echo", "hello"]
    );
    assert!(!ir_task.shell);
    assert_eq!(ir_task.priority, Some(10));
    assert_eq!(ir_task.phase, Some(BuildStage::Setup)); // priority 10 = Setup
}

#[test]
fn test_contributor_task_to_ir_script() {
    let contributor_task = ContributorTask {
        id: "script-task".to_string(),
        label: None,
        description: None,
        command: None,
        args: vec![],
        script: Some("echo line1\necho line2".to_string()),
        shell: true,
        env: HashMap::default(),
        secrets: HashMap::default(),
        inputs: vec![],
        outputs: vec![],
        hermetic: false,
        depends_on: vec!["other".to_string()],
        priority: 5,
        condition: None,
        provider: None,
    };

    let ir_task = test_compiler().contributor_task_to_ir(&contributor_task, "github");

    assert_eq!(ir_task.id, "cuenv:contributor:script-task");
    assert_eq!(ir_task.command, vec!["echo line1\necho line2"]);
    assert!(ir_task.shell);
    assert_eq!(ir_task.depends_on, vec!["cuenv:contributor:other"]);
    assert_eq!(ir_task.priority, Some(5));
    assert_eq!(ir_task.phase, Some(BuildStage::Bootstrap)); // priority 5 = Bootstrap
}

#[test]
fn test_contributor_task_to_ir_github_action() {
    use cuenv_core::ci::{GitHubActionConfig, TaskProviderConfig};

    let mut inputs = std::collections::BTreeMap::new();
    inputs.insert(
        "extra-conf".to_string(),
        serde_json::Value::String("accept-flake-config = true".to_string()),
    );

    let contributor_task = ContributorTask {
        id: "nix.install".to_string(),
        label: Some("Install Nix".to_string()),
        description: None,
        command: None,
        args: vec![],
        script: None,
        shell: false,
        env: HashMap::default(),
        secrets: HashMap::default(),
        inputs: vec![],
        outputs: vec![],
        hermetic: false,
        depends_on: vec![],
        priority: 0,
        condition: None,
        provider: Some(TaskProviderConfig {
            github: Some(GitHubActionConfig {
                uses: "DeterminateSystems/determinate-nix-action@v3".to_string(),
                inputs,
                if_condition: Some("runner.os == 'Linux'".to_string()),
            }),
        }),
    };

    let ir_task = test_compiler().contributor_task_to_ir(&contributor_task, "nix");

    assert_eq!(ir_task.id, "cuenv:contributor:nix.install");
    assert!(ir_task.command.is_empty()); // No command, uses action
    assert!(ir_task.provider_hints.is_some());
    assert_eq!(ir_task.phase, Some(BuildStage::Bootstrap)); // priority 0 = Bootstrap

    // Verify the GitHub action is in provider_hints
    let hints = ir_task.provider_hints.as_ref().unwrap();
    let github_action = hints.get("github_action").unwrap();
    assert_eq!(
        github_action.get("uses").and_then(|v| v.as_str()),
        Some("DeterminateSystems/determinate-nix-action@v3")
    );
    assert_eq!(
        github_action.get("if").and_then(|v| v.as_str()),
        Some("runner.os == 'Linux'")
    );
}

#[test]
fn test_contributor_task_to_ir_secrets() {
    use cuenv_core::ci::SecretRefConfig;

    let mut secrets = std::collections::HashMap::new();
    secrets.insert(
        "SIMPLE_SECRET".to_string(),
        SecretRef::Simple("SECRET_NAME".to_string()),
    );
    secrets.insert(
        "DETAILED_SECRET".to_string(),
        SecretRef::Detailed(SecretRefConfig {
            source: "DETAILED_SOURCE".to_string(),
            cache_key: true,
        }),
    );

    let contributor_task = ContributorTask {
        id: "secrets-task".to_string(),
        label: None,
        description: None,
        command: Some("echo".to_string()),
        args: vec!["test".to_string()],
        script: None,
        shell: false,
        env: HashMap::default(),
        secrets,
        inputs: vec![],
        outputs: vec![],
        hermetic: false,
        depends_on: vec![],
        priority: 10,
        condition: None,
        provider: None,
    };

    let ir_task = test_compiler().contributor_task_to_ir(&contributor_task, "github");

    assert_eq!(ir_task.secrets.len(), 2);
    assert_eq!(ir_task.phase, Some(BuildStage::Setup));

    // Check simple secret conversion
    let simple = ir_task.secrets.get("SIMPLE_SECRET").unwrap();
    assert_eq!(simple.source, "SECRET_NAME");
    assert!(!simple.cache_key);

    // Check detailed secret conversion
    let detailed = ir_task.secrets.get("DETAILED_SECRET").unwrap();
    assert_eq!(detailed.source, "DETAILED_SOURCE");
    assert!(detailed.cache_key);
}

#[test]
fn test_contributor_task_to_ir_env_vars() {
    let mut env = std::collections::HashMap::new();
    env.insert("VAR1".to_string(), "value1".to_string());
    env.insert("VAR2".to_string(), "value2".to_string());

    let contributor_task = ContributorTask {
        id: "env-task".to_string(),
        label: None,
        description: None,
        command: Some("printenv".to_string()),
        args: vec![],
        script: None,
        shell: false,
        env,
        secrets: HashMap::default(),
        inputs: vec![],
        outputs: vec![],
        hermetic: false,
        depends_on: vec![],
        priority: 10,
        condition: None,
        provider: None,
    };

    let ir_task = test_compiler().contributor_task_to_ir(&contributor_task, "github");

    assert_eq!(ir_task.env.len(), 2);
    assert_eq!(ir_task.env.get("VAR1"), Some(&"value1".to_string()));
    assert_eq!(ir_task.env.get("VAR2"), Some(&"value2".to_string()));
    assert_eq!(ir_task.phase, Some(BuildStage::Setup));
}

#[test]
fn test_contributor_task_to_ir_command_with_args() {
    let contributor_task = ContributorTask {
        id: "bun.workspace.install".to_string(),
        label: Some("Install Bun Dependencies".to_string()),
        description: None,
        command: Some("bun".to_string()),
        args: vec!["install".to_string(), "--frozen-lockfile".to_string()],
        script: None,
        shell: false,
        env: HashMap::default(),
        secrets: HashMap::default(),
        inputs: vec!["package.json".to_string(), "bun.lock".to_string()],
        outputs: vec![],
        hermetic: false,
        depends_on: vec![],
        priority: 10,
        condition: None,
        provider: None,
    };

    let ir_task = test_compiler().contributor_task_to_ir(&contributor_task, "bun.workspace");

    assert_eq!(ir_task.id, "cuenv:contributor:bun.workspace.install");
    // Commands are wrapped with cuenv exec for tool activation
    assert_eq!(
        ir_task.command,
        vec!["cuenv", "exec", "--", "bun", "install", "--frozen-lockfile"]
    );
    assert!(!ir_task.shell);
    assert_eq!(ir_task.phase, Some(BuildStage::Setup));
    assert_eq!(ir_task.inputs, vec!["package.json", "bun.lock"]);
}

#[test]
fn test_contributor_task_to_ir_cuenv_contributor_not_wrapped() {
    // Tasks from the cuenv contributor should NOT be wrapped with cuenv exec
    // because they are setting up cuenv itself
    let contributor_task = ContributorTask {
        id: "cuenv.setup".to_string(),
        label: Some("Setup cuenv".to_string()),
        description: None,
        command: Some("brew".to_string()),
        args: vec!["install".to_string(), "cuenv/cuenv/cuenv".to_string()],
        script: None,
        shell: false,
        env: HashMap::default(),
        secrets: HashMap::default(),
        inputs: vec![],
        outputs: vec![],
        hermetic: false,
        depends_on: vec![],
        priority: 10,
        condition: None,
        provider: None,
    };

    let ir_task = test_compiler().contributor_task_to_ir(&contributor_task, "cuenv");

    assert_eq!(ir_task.id, "cuenv:contributor:cuenv.setup");
    // Should NOT be wrapped - cuenv contributor tasks set up cuenv itself
    assert_eq!(
        ir_task.command,
        vec!["brew", "install", "cuenv/cuenv/cuenv"]
    );
}

#[test]
fn test_contributor_task_to_ir_bootstrap_not_wrapped() {
    // Bootstrap phase tasks (priority < 10) should NOT be wrapped with cuenv exec
    // because they run before cuenv is built
    let contributor_task = ContributorTask {
        id: "setup.rust".to_string(),
        label: Some("Setup Rust".to_string()),
        description: None,
        command: Some("rustup".to_string()),
        args: vec!["default".to_string(), "stable".to_string()],
        script: None,
        shell: false,
        env: HashMap::default(),
        secrets: HashMap::default(),
        inputs: vec![],
        outputs: vec![],
        hermetic: false,
        depends_on: vec![],
        priority: 6, // Bootstrap phase
        condition: None,
        provider: None,
    };

    let ir_task = test_compiler().contributor_task_to_ir(&contributor_task, "rust");

    assert_eq!(ir_task.id, "cuenv:contributor:setup.rust");
    // Should NOT be wrapped - bootstrap tasks run before cuenv.setup
    assert_eq!(ir_task.command, vec!["rustup", "default", "stable"]);
    assert_eq!(ir_task.phase, Some(BuildStage::Bootstrap));
}

#[test]
fn test_derive_stage_from_priority_bootstrap() {
    // Priority 0-9 = Bootstrap
    assert_eq!(
        Compiler::derive_stage_from_priority(0, None),
        BuildStage::Bootstrap
    );
    assert_eq!(
        Compiler::derive_stage_from_priority(5, None),
        BuildStage::Bootstrap
    );
    assert_eq!(
        Compiler::derive_stage_from_priority(9, None),
        BuildStage::Bootstrap
    );
}

#[test]
fn test_derive_stage_from_priority_setup() {
    // Priority 10-49 = Setup
    assert_eq!(
        Compiler::derive_stage_from_priority(10, None),
        BuildStage::Setup
    );
    assert_eq!(
        Compiler::derive_stage_from_priority(25, None),
        BuildStage::Setup
    );
    assert_eq!(
        Compiler::derive_stage_from_priority(49, None),
        BuildStage::Setup
    );
}

#[test]
fn test_derive_stage_from_priority_success() {
    // Priority 50+ = Success
    assert_eq!(
        Compiler::derive_stage_from_priority(50, None),
        BuildStage::Success
    );
    assert_eq!(
        Compiler::derive_stage_from_priority(100, None),
        BuildStage::Success
    );
}

#[test]
fn test_derive_stage_from_priority_failure_condition() {
    // on_failure condition = Failure regardless of priority
    assert_eq!(
        Compiler::derive_stage_from_priority(0, Some(CueTaskCondition::OnFailure)),
        BuildStage::Failure
    );
    assert_eq!(
        Compiler::derive_stage_from_priority(50, Some(CueTaskCondition::OnFailure)),
        BuildStage::Failure
    );
}

// Tests for cue_task_condition_to_ir
#[test]
fn test_cue_task_condition_to_ir_on_success() {
    let result = Compiler::cue_task_condition_to_ir(CueTaskCondition::OnSuccess);
    assert_eq!(result, TaskCondition::OnSuccess);
}

#[test]
fn test_cue_task_condition_to_ir_on_failure() {
    let result = Compiler::cue_task_condition_to_ir(CueTaskCondition::OnFailure);
    assert_eq!(result, TaskCondition::OnFailure);
}

#[test]
fn test_cue_task_condition_to_ir_always() {
    let result = Compiler::cue_task_condition_to_ir(CueTaskCondition::Always);
    assert_eq!(result, TaskCondition::Always);
}
