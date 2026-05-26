use super::*;
use crate::ir::{BuildStage, TaskCondition};
use cuenv_core::ci::{
    ActivationCondition, CI, Contributor, ContributorTask, Pipeline, PipelineCondition,
    PipelineMode, PipelineTask, SecretRef, StringOrVec, TaskCondition as CueTaskCondition, TaskRef,
};
use cuenv_core::tasks::{Input, Task, TaskDependency, TaskNode};

fn test_compiler() -> Compiler {
    Compiler::new(Project::new("test-project"))
}

fn trigger_paths_for_project(project_path: &str, inputs: &[&str]) -> Vec<String> {
    let mut project = Project::new("test-project");
    project.tasks.insert(
        "build".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "cargo".to_string(),
            args: vec!["build".to_string()],
            inputs: inputs
                .iter()
                .map(|input| Input::Path((*input).to_string()))
                .collect(),
            ..Default::default()
        })),
    );

    let pipeline = Pipeline {
        tasks: vec![PipelineTask::Simple(TaskRef::from_name("build"))],
        when: Some(PipelineCondition {
            branch: Some(StringOrVec::String("main".to_string())),
            pull_request: None,
            tag: None,
            default_branch: None,
            scheduled: None,
            manual: None,
            release: None,
        }),
        ..Default::default()
    };

    project.ci = Some(CI {
        pipelines: BTreeMap::from([("default".to_string(), pipeline.clone())]),
        ..Default::default()
    });

    let options = CompilerOptions {
        pipeline_name: Some("default".to_string()),
        pipeline: Some(pipeline),
        project_path: Some(project_path.to_string()),
        ..Default::default()
    };

    let compiler = Compiler::with_options(project, options);
    let ir = compiler.compile().unwrap();

    ir.pipeline.trigger.expect("should have trigger").paths
}

#[test]
fn test_compile_simple_task() {
    let mut project = Project::new("test-project");
    project.tasks.insert(
        "build".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "cargo".to_string(),
            args: vec!["build".to_string()],
            inputs: vec![cuenv_core::tasks::Input::Path("src/**/*.rs".to_string())],
            outputs: vec!["target/debug/binary".to_string()],
            ..Default::default()
        })),
    );

    let compiler = Compiler::new(project);
    let ir = compiler.compile().unwrap();

    assert_eq!(ir.version, "1.5");
    assert_eq!(ir.pipeline.name, "test-project");
    assert_eq!(ir.tasks.len(), 1);
    assert_eq!(ir.tasks[0].id, "build");
    assert_eq!(ir.tasks[0].command, vec!["cargo", "build"]);
    assert_eq!(ir.tasks[0].inputs, vec!["src/**/*.rs"]);
}

#[test]
fn test_compile_task_with_dependencies() {
    let mut project = Project::new("test-project");

    project.tasks.insert(
        "test".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "cargo".to_string(),
            args: vec!["test".to_string()],
            depends_on: vec![TaskDependency::from_name("build")],
            ..Default::default()
        })),
    );

    project.tasks.insert(
        "build".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "cargo".to_string(),
            args: vec!["build".to_string()],
            ..Default::default()
        })),
    );

    let compiler = Compiler::new(project);
    let ir = compiler.compile().unwrap();

    assert_eq!(ir.tasks.len(), 2);

    let test_task = ir.tasks.iter().find(|t| t.id == "test").unwrap();
    assert_eq!(test_task.depends_on, vec!["build"]);
}

#[test]
fn test_compile_deployment_task() {
    let mut project = Project::new("test-project");

    project.tasks.insert(
        "deploy".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "kubectl".to_string(),
            args: vec!["apply".to_string()],
            labels: vec!["deployment".to_string()],
            ..Default::default()
        })),
    );

    let compiler = Compiler::new(project);
    let ir = compiler.compile().unwrap();

    assert_eq!(ir.tasks.len(), 1);
    assert!(ir.tasks[0].deployment);
    assert_eq!(ir.tasks[0].cache_policy, CachePolicy::Disabled);
}

#[test]
fn test_compile_script_task() {
    let mut project = Project::new("test-project");

    project.tasks.insert(
        "script-task".to_string(),
        TaskNode::Task(Box::new(Task {
            script: Some("echo 'Running script'\nls -la".to_string()),
            ..Default::default()
        })),
    );

    let compiler = Compiler::new(project);
    let ir = compiler.compile().unwrap();

    assert_eq!(ir.tasks.len(), 1);
    assert!(ir.tasks[0].shell);
    assert_eq!(ir.tasks[0].command[0], "/bin/sh");
    assert_eq!(ir.tasks[0].command[1], "-c");
}

#[test]
fn test_purity_analysis_pure_flake() {
    use std::io::Write;
    use tempfile::NamedTempFile;

    let json = r#"{
            "nodes": {
                "nixpkgs": {
                    "locked": {
                        "type": "github",
                        "owner": "NixOS",
                        "repo": "nixpkgs",
                        "rev": "abc123",
                        "narHash": "sha256-xxxxxxxxxxxxx"
                    }
                },
                "root": { "inputs": { "nixpkgs": "nixpkgs" } }
            },
            "root": "root",
            "version": 7
        }"#;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(json.as_bytes()).unwrap();

    let project = Project::new("test-project");
    let options = CompilerOptions {
        purity_mode: PurityMode::Strict,
        flake_lock_path: Some(temp_file.path().to_path_buf()),
        ..Default::default()
    };

    let compiler = Compiler::with_options(project, options);
    let result = compiler.analyze_flake_purity();

    assert!(result.is_some());
    let (digest, purity) = result.unwrap().unwrap();
    assert!(digest.starts_with("sha256:"));
    assert_eq!(purity, PurityMode::Strict);
}

#[test]
fn test_purity_strict_mode_rejects_unlocked() {
    use std::io::Write;
    use tempfile::NamedTempFile;

    let json = r#"{
            "nodes": {
                "nixpkgs": {
                    "original": { "type": "github", "owner": "NixOS", "repo": "nixpkgs" }
                },
                "root": { "inputs": { "nixpkgs": "nixpkgs" } }
            },
            "root": "root",
            "version": 7
        }"#;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(json.as_bytes()).unwrap();

    let project = Project::new("test-project");
    let options = CompilerOptions {
        purity_mode: PurityMode::Strict,
        flake_lock_path: Some(temp_file.path().to_path_buf()),
        ..Default::default()
    };

    let compiler = Compiler::with_options(project, options);
    let result = compiler.analyze_flake_purity();

    assert!(result.is_some());
    assert!(result.unwrap().is_err());
}

#[test]
fn test_purity_warning_mode_injects_uuid() {
    use std::io::Write;
    use tempfile::NamedTempFile;

    let json = r#"{
            "nodes": {
                "nixpkgs": {
                    "original": { "type": "github", "owner": "NixOS", "repo": "nixpkgs" }
                },
                "root": { "inputs": { "nixpkgs": "nixpkgs" } }
            },
            "root": "root",
            "version": 7
        }"#;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(json.as_bytes()).unwrap();

    let project = Project::new("test-project");
    let options = CompilerOptions {
        purity_mode: PurityMode::Warning,
        flake_lock_path: Some(temp_file.path().to_path_buf()),
        ..Default::default()
    };

    let compiler = Compiler::with_options(project.clone(), options.clone());
    let result1 = compiler.analyze_flake_purity().unwrap().unwrap();

    let compiler2 = Compiler::with_options(project, options);
    let result2 = compiler2.analyze_flake_purity().unwrap().unwrap();

    // Each compile should produce different digests due to UUID injection
    assert_ne!(result1.0, result2.0);
    assert_eq!(result1.1, PurityMode::Warning);
}

#[test]
fn test_purity_override_mode_uses_overrides() {
    use std::io::Write;
    use tempfile::NamedTempFile;

    let json = r#"{
            "nodes": {
                "nixpkgs": {
                    "locked": {
                        "type": "github",
                        "narHash": "sha256-base"
                    }
                },
                "root": { "inputs": { "nixpkgs": "nixpkgs" } }
            },
            "root": "root",
            "version": 7
        }"#;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(json.as_bytes()).unwrap();

    let mut input_overrides = HashMap::new();
    input_overrides.insert("nixpkgs".to_string(), "sha256-custom".to_string());

    let project = Project::new("test-project");
    let options = CompilerOptions {
        purity_mode: PurityMode::Override,
        flake_lock_path: Some(temp_file.path().to_path_buf()),
        input_overrides,
        ..Default::default()
    };

    let compiler = Compiler::with_options(project.clone(), options.clone());
    let result1 = compiler.analyze_flake_purity().unwrap().unwrap();

    // Same compiler, same overrides = deterministic digest
    let compiler2 = Compiler::with_options(project, options);
    let result2 = compiler2.analyze_flake_purity().unwrap().unwrap();

    assert_eq!(result1.0, result2.0);
    assert_eq!(result1.1, PurityMode::Override);
}

#[test]
fn test_compute_runtime() {
    use std::io::Write;
    use tempfile::NamedTempFile;

    let json = r#"{
            "nodes": {
                "nixpkgs": {
                    "locked": {
                        "type": "github",
                        "narHash": "sha256-test"
                    }
                },
                "root": { "inputs": { "nixpkgs": "nixpkgs" } }
            },
            "root": "root",
            "version": 7
        }"#;

    let mut temp_file = NamedTempFile::new().unwrap();
    temp_file.write_all(json.as_bytes()).unwrap();

    let project = Project::new("test-project");
    let options = CompilerOptions {
        purity_mode: PurityMode::Strict,
        flake_lock_path: Some(temp_file.path().to_path_buf()),
        ..Default::default()
    };

    let compiler = Compiler::with_options(project, options);
    let runtime = compiler
        .compute_runtime(
            "nix-x86_64-linux",
            "github:NixOS/nixpkgs",
            "devShells.x86_64-linux.default",
            "x86_64-linux",
        )
        .unwrap();

    assert_eq!(runtime.id, "nix-x86_64-linux");
    assert_eq!(runtime.flake, "github:NixOS/nixpkgs");
    assert!(runtime.digest.starts_with("sha256:"));
    assert_eq!(runtime.purity, PurityMode::Strict);
}

#[test]
fn test_derive_trigger_paths_with_project_path() {
    use cuenv_core::ci::{CI, Pipeline, PipelineCondition, PipelineTask, StringOrVec, TaskRef};
    use std::collections::BTreeMap;

    let mut project = Project::new("test-project");
    project.tasks.insert(
        "build".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "cargo".to_string(),
            args: vec!["build".to_string()],
            inputs: vec![
                cuenv_core::tasks::Input::Path("src/**/*.rs".to_string()),
                cuenv_core::tasks::Input::Path("Cargo.toml".to_string()),
            ],
            ..Default::default()
        })),
    );

    let pipeline = Pipeline {
        tasks: vec![PipelineTask::Simple(TaskRef::from_name("build"))],
        when: Some(PipelineCondition {
            branch: Some(StringOrVec::String("main".to_string())),
            pull_request: None,
            tag: None,
            default_branch: None,
            scheduled: None,
            manual: None,
            release: None,
        }),
        ..Default::default()
    };

    // Add CI config with a pipeline
    project.ci = Some(CI {
        pipelines: BTreeMap::from([("default".to_string(), pipeline.clone())]),
        ..Default::default()
    });

    let options = CompilerOptions {
        pipeline_name: Some("default".to_string()),
        pipeline: Some(pipeline),
        project_path: Some("projects/api".to_string()),
        ..Default::default()
    };

    let compiler = Compiler::with_options(project, options);
    let ir = compiler.compile().unwrap();

    let trigger = ir.pipeline.trigger.expect("should have trigger");

    // Task inputs should be prefixed with project_path
    assert!(
        trigger
            .paths
            .contains(&"projects/api/src/**/*.rs".to_string())
    );
    assert!(
        trigger
            .paths
            .contains(&"projects/api/Cargo.toml".to_string())
    );

    // CUE implicit paths should also be prefixed
    assert!(trigger.paths.contains(&"projects/api/env.cue".to_string()));
    assert!(
        trigger
            .paths
            .contains(&"projects/api/schema/**".to_string())
    );

    // cue.mod should NOT be prefixed (it's at module root)
    assert!(trigger.paths.contains(&"cue.mod/**".to_string()));
}

#[test]
fn test_derive_trigger_paths_fallback_to_project_dir() {
    use cuenv_core::ci::{CI, Pipeline, PipelineCondition, PipelineTask, StringOrVec, TaskRef};
    use std::collections::BTreeMap;

    let mut project = Project::new("test-project");
    // Task with NO inputs
    project.tasks.insert(
        "deploy".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "kubectl".to_string(),
            args: vec!["apply".to_string()],
            ..Default::default()
        })),
    );

    let pipeline = Pipeline {
        tasks: vec![PipelineTask::Simple(TaskRef::from_name("deploy"))],
        when: Some(PipelineCondition {
            branch: Some(StringOrVec::String("main".to_string())),
            pull_request: None,
            tag: None,
            default_branch: None,
            scheduled: None,
            manual: None,
            release: None,
        }),
        ..Default::default()
    };

    project.ci = Some(CI {
        pipelines: BTreeMap::from([("default".to_string(), pipeline.clone())]),
        ..Default::default()
    });

    let options = CompilerOptions {
        pipeline_name: Some("default".to_string()),
        pipeline: Some(pipeline),
        project_path: Some("projects/rawkode.academy/api".to_string()),
        ..Default::default()
    };

    let compiler = Compiler::with_options(project, options);
    let ir = compiler.compile().unwrap();

    let trigger = ir.pipeline.trigger.expect("should have trigger");

    // When no task inputs, should fallback to project directory
    assert!(
        trigger
            .paths
            .contains(&"projects/rawkode.academy/api/**".to_string()),
        "Should contain fallback path. Paths: {:?}",
        trigger.paths
    );
}

#[test]
fn test_derive_trigger_paths_root_project() {
    use cuenv_core::ci::{CI, Pipeline, PipelineCondition, PipelineTask, StringOrVec, TaskRef};
    use std::collections::BTreeMap;

    let mut project = Project::new("test-project");
    project.tasks.insert(
        "build".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "cargo".to_string(),
            args: vec!["build".to_string()],
            inputs: vec![cuenv_core::tasks::Input::Path("src/**".to_string())],
            ..Default::default()
        })),
    );

    let pipeline = Pipeline {
        tasks: vec![PipelineTask::Simple(TaskRef::from_name("build"))],
        when: Some(PipelineCondition {
            branch: Some(StringOrVec::String("main".to_string())),
            pull_request: None,
            tag: None,
            default_branch: None,
            scheduled: None,
            manual: None,
            release: None,
        }),
        ..Default::default()
    };

    project.ci = Some(CI {
        pipelines: BTreeMap::from([("default".to_string(), pipeline.clone())]),
        ..Default::default()
    });

    // No project_path = root project
    let options = CompilerOptions {
        pipeline_name: Some("default".to_string()),
        pipeline: Some(pipeline),
        project_path: None,
        ..Default::default()
    };

    let compiler = Compiler::with_options(project, options);
    let ir = compiler.compile().unwrap();

    let trigger = ir.pipeline.trigger.expect("should have trigger");

    // Paths should NOT be prefixed for root projects
    assert!(trigger.paths.contains(&"src/**".to_string()));
    assert!(trigger.paths.contains(&"env.cue".to_string()));
    assert!(trigger.paths.contains(&"schema/**".to_string()));
}

#[test]
fn test_derive_trigger_paths_root_project_no_inputs_fallback() {
    use cuenv_core::ci::{CI, Pipeline, PipelineCondition, PipelineTask, StringOrVec, TaskRef};
    use std::collections::BTreeMap;

    let mut project = Project::new("test-project");
    // Task with NO inputs
    project.tasks.insert(
        "deploy".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "kubectl".to_string(),
            args: vec!["apply".to_string()],
            ..Default::default()
        })),
    );

    let pipeline = Pipeline {
        tasks: vec![PipelineTask::Simple(TaskRef::from_name("deploy"))],
        when: Some(PipelineCondition {
            branch: Some(StringOrVec::String("main".to_string())),
            pull_request: None,
            tag: None,
            default_branch: None,
            scheduled: None,
            manual: None,
            release: None,
        }),
        ..Default::default()
    };

    project.ci = Some(CI {
        pipelines: BTreeMap::from([("default".to_string(), pipeline.clone())]),
        ..Default::default()
    });

    // No project_path = root project
    let options = CompilerOptions {
        pipeline_name: Some("default".to_string()),
        pipeline: Some(pipeline),
        project_path: None,
        ..Default::default()
    };

    let compiler = Compiler::with_options(project, options);
    let ir = compiler.compile().unwrap();

    let trigger = ir.pipeline.trigger.expect("should have trigger");

    // Root project with no inputs should fallback to **
    assert!(
        trigger.paths.contains(&"**".to_string()),
        "Root project with no inputs should fallback to **. Paths: {:?}",
        trigger.paths
    );
}

// =========================================================================
// Contributor Activation Tests
// =========================================================================

use std::collections::HashMap;

/// Helper to create a minimal Contributor for testing
fn test_contributor(id: &str, when: Option<ActivationCondition>) -> Contributor {
    Contributor {
        id: id.to_string(),
        when,
        tasks: vec![],
        auto_associate: None,
    }
}

/// Helper to create a minimal IR for testing
fn test_ir() -> IntermediateRepresentation {
    IntermediateRepresentation {
        version: "1.5".to_string(),
        pipeline: crate::ir::PipelineMetadata {
            name: "test".to_string(),
            mode: PipelineMode::default(),
            environment: None,
            requires_onepassword: false,
            project_name: None,
            project_path: None,
            trigger: None,
            pipeline_tasks: vec![],
            pipeline_task_defs: vec![],
        },
        runtimes: vec![],
        tasks: vec![],
    }
}

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

// =========================================================================
// Path Derivation Tests
// =========================================================================

#[test]
fn test_derive_paths_from_task_group() {
    // Create a task group (like "check" with nested tasks "lint", "test", etc.)
    let mut project = Project::new("test-project");

    let mut group_tasks = HashMap::new();
    group_tasks.insert(
        "lint".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "cargo".to_string(),
            args: vec!["clippy".to_string()],
            inputs: vec![
                Input::Path("Cargo.toml".to_string()),
                Input::Path("crates/**".to_string()),
            ],
            ..Default::default()
        })),
    );
    group_tasks.insert(
        "test".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "cargo".to_string(),
            args: vec!["test".to_string()],
            inputs: vec![
                Input::Path("Cargo.toml".to_string()),
                Input::Path("crates/**".to_string()),
                Input::Path("tests/**".to_string()),
            ],
            ..Default::default()
        })),
    );

    project.tasks.insert(
        "check".to_string(),
        TaskNode::Group(TaskGroup {
            type_: "group".to_string(),
            children: group_tasks,
            depends_on: vec![],
            description: None,
            max_concurrency: None,
        }),
    );

    let pipeline = Pipeline {
        tasks: vec![PipelineTask::Simple(TaskRef::from_name("check"))],
        when: Some(PipelineCondition {
            branch: Some(StringOrVec::String("main".to_string())),
            pull_request: None,
            tag: None,
            default_branch: None,
            scheduled: None,
            manual: None,
            release: None,
        }),
        ..Default::default()
    };

    project.ci = Some(CI {
        pipelines: BTreeMap::from([("default".to_string(), pipeline.clone())]),
        ..Default::default()
    });

    // Root project (no project_path prefix)
    let options = CompilerOptions {
        pipeline_name: Some("default".to_string()),
        pipeline: Some(pipeline),
        project_path: None,
        ..Default::default()
    };

    let compiler = Compiler::with_options(project, options);
    let ir = compiler.compile().unwrap();

    let trigger = ir.pipeline.trigger.expect("should have trigger");

    // Should collect inputs from all nested tasks in the group
    assert!(
        trigger.paths.contains(&"Cargo.toml".to_string()),
        "Should contain Cargo.toml from group tasks. Paths: {:?}",
        trigger.paths
    );
    assert!(
        trigger.paths.contains(&"crates/**".to_string()),
        "Should contain crates/** from group tasks. Paths: {:?}",
        trigger.paths
    );
    assert!(
        trigger.paths.contains(&"tests/**".to_string()),
        "Should contain tests/** from group tasks. Paths: {:?}",
        trigger.paths
    );
    // Should NOT fallback to ** since we have inputs
    assert!(
        !trigger.paths.contains(&"**".to_string()),
        "Should not fallback to ** when task group has inputs. Paths: {:?}",
        trigger.paths
    );
}

#[test]
fn test_derive_paths_root_project_no_dot_prefix() {
    // When project_path is "." (root), paths should not have "./" prefix
    let mut project = Project::new("test-project");

    project.tasks.insert(
        "build".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "cargo".to_string(),
            args: vec!["build".to_string()],
            inputs: vec![Input::Path("src/**".to_string())],
            ..Default::default()
        })),
    );

    let pipeline = Pipeline {
        tasks: vec![PipelineTask::Simple(TaskRef::from_name("build"))],
        when: Some(PipelineCondition {
            branch: Some(StringOrVec::String("main".to_string())),
            pull_request: None,
            tag: None,
            default_branch: None,
            scheduled: None,
            manual: None,
            release: None,
        }),
        ..Default::default()
    };

    project.ci = Some(CI {
        pipelines: BTreeMap::from([("default".to_string(), pipeline.clone())]),
        ..Default::default()
    });

    // project_path = "." (root project, as set by sync command)
    let options = CompilerOptions {
        pipeline_name: Some("default".to_string()),
        pipeline: Some(pipeline),
        project_path: Some(".".to_string()),
        ..Default::default()
    };

    let compiler = Compiler::with_options(project, options);
    let ir = compiler.compile().unwrap();

    let trigger = ir.pipeline.trigger.expect("should have trigger");

    // Paths should NOT have "./" prefix - GitHub Actions doesn't handle it correctly
    assert!(
        trigger.paths.contains(&"src/**".to_string()),
        "Should contain src/** without ./ prefix. Paths: {:?}",
        trigger.paths
    );
    assert!(
        !trigger.paths.iter().any(|p| p.starts_with("./")),
        "No path should have ./ prefix. Paths: {:?}",
        trigger.paths
    );
    assert!(
        trigger.paths.contains(&"env.cue".to_string()),
        "Should contain env.cue without ./ prefix. Paths: {:?}",
        trigger.paths
    );
}

#[test]
fn test_derive_paths_subproject_has_prefix() {
    // When project_path is "projects/api", paths should be prefixed
    let mut project = Project::new("test-project");

    project.tasks.insert(
        "build".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "cargo".to_string(),
            args: vec!["build".to_string()],
            inputs: vec![Input::Path("src/**".to_string())],
            ..Default::default()
        })),
    );

    let pipeline = Pipeline {
        tasks: vec![PipelineTask::Simple(TaskRef::from_name("build"))],
        when: Some(PipelineCondition {
            branch: Some(StringOrVec::String("main".to_string())),
            pull_request: None,
            tag: None,
            default_branch: None,
            scheduled: None,
            manual: None,
            release: None,
        }),
        ..Default::default()
    };

    project.ci = Some(CI {
        pipelines: BTreeMap::from([("default".to_string(), pipeline.clone())]),
        ..Default::default()
    });

    // Subproject path
    let options = CompilerOptions {
        pipeline_name: Some("default".to_string()),
        pipeline: Some(pipeline),
        project_path: Some("projects/api".to_string()),
        ..Default::default()
    };

    let compiler = Compiler::with_options(project, options);
    let ir = compiler.compile().unwrap();

    let trigger = ir.pipeline.trigger.expect("should have trigger");

    // Paths should have the project prefix
    assert!(
        trigger.paths.contains(&"projects/api/src/**".to_string()),
        "Should contain prefixed path. Paths: {:?}",
        trigger.paths
    );
    assert!(
        trigger.paths.contains(&"projects/api/env.cue".to_string()),
        "Should contain prefixed env.cue. Paths: {:?}",
        trigger.paths
    );
}

#[test]
fn test_derive_paths_nested_project_normalizes_parent_inputs() {
    let paths = trigger_paths_for_project(
        "server",
        &[
            "../flake.nix",
            "../infrastructure/waddle.cloud/gitops/waddle-server/**",
            "src/**",
        ],
    );

    assert!(paths.contains(&"flake.nix".to_string()), "Paths: {paths:?}");
    assert!(
        paths.contains(&"infrastructure/waddle.cloud/gitops/waddle-server/**".to_string()),
        "Paths: {paths:?}"
    );
    assert!(
        paths.contains(&"server/src/**".to_string()),
        "Paths: {paths:?}"
    );
    assert!(
        paths.contains(&"server/env.cue".to_string()),
        "Paths: {paths:?}"
    );
    assert!(
        paths.contains(&"server/schema/**".to_string()),
        "Paths: {paths:?}"
    );
    assert!(
        paths.iter().all(|path| !path.contains("../")),
        "Paths should not contain parent traversal: {paths:?}"
    );
}

#[test]
fn test_derive_paths_deep_nested_project_normalizes_parent_inputs() {
    let paths = trigger_paths_for_project("apps/server", &["../../flake.nix", "../shared/**"]);

    assert!(paths.contains(&"flake.nix".to_string()), "Paths: {paths:?}");
    assert!(
        paths.contains(&"apps/shared/**".to_string()),
        "Paths: {paths:?}"
    );
    assert!(
        paths.contains(&"apps/server/env.cue".to_string()),
        "Paths: {paths:?}"
    );
    assert!(
        paths.iter().all(|path| !path.contains("../")),
        "Paths should not contain parent traversal: {paths:?}"
    );
}

#[test]
fn test_derive_paths_skips_inputs_that_escape_repo_root() {
    let paths = trigger_paths_for_project("server", &["../../outside/**"]);

    assert!(
        !paths.iter().any(|path| path.contains("outside")),
        "Escaping paths should be skipped: {paths:?}"
    );
    assert!(
        paths.iter().all(|path| !path.contains("..")),
        "Paths should not contain parent traversal: {paths:?}"
    );
    assert!(
        !paths.contains(&"server/**".to_string()),
        "Escaping task input should not trigger project fallback: {paths:?}"
    );
}

#[test]
fn test_derive_paths_emits_recursive_glob_for_simple_directory_inputs() {
    // Inputs without glob metacharacters can refer to files or directories.
    // GitHub Actions path filters do not treat a bare `server/src` as
    // matching files beneath it, so we emit `<path>/**` alongside the
    // literal path. This keeps derived filters aligned with
    // `cuenv_core::affected::matches_pattern`, which treats non-glob
    // patterns as prefixes.
    let paths = trigger_paths_for_project("server", &["src", "../flake.nix"]);

    assert!(
        paths.contains(&"server/src".to_string()),
        "literal path should still be emitted: {paths:?}"
    );
    assert!(
        paths.contains(&"server/src/**".to_string()),
        "directory input should also emit recursive glob: {paths:?}"
    );
    assert!(
        paths.contains(&"flake.nix".to_string()),
        "literal file from parent should be emitted: {paths:?}"
    );
    assert!(
        paths.contains(&"flake.nix/**".to_string()),
        "simple parent path should also emit recursive glob (covers \
             the case where it turns out to be a directory): {paths:?}"
    );
}

#[test]
fn test_derive_paths_does_not_expand_existing_globs() {
    // Inputs that already contain glob metacharacters must not get a
    // duplicate `/**` appended; their meaning is left to GitHub Actions.
    let paths = trigger_paths_for_project("server", &["src/**/*.rs", "data/?.json"]);

    assert!(
        paths.contains(&"server/src/**/*.rs".to_string()),
        "glob input should be emitted as-is: {paths:?}"
    );
    assert!(
        !paths.iter().any(|p| p == "server/src/**/*.rs/**"),
        "glob input should not have /** appended: {paths:?}"
    );
    assert!(
        paths.contains(&"server/data/?.json".to_string()),
        "wildcard input should be emitted as-is: {paths:?}"
    );
    assert!(
        !paths.iter().any(|p| p == "server/data/?.json/**"),
        "wildcard input should not have /** appended: {paths:?}"
    );
}

#[test]
fn test_expand_dependency_to_task_group() {
    // Test that dependencies on task groups are expanded to their leaf tasks
    let mut project = Project::new("test-project");

    // Create a task group with children
    let mut test_children = HashMap::new();
    test_children.insert(
        "unit".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "cargo".to_string(),
            args: vec!["test".to_string(), "--lib".to_string()],
            ..Default::default()
        })),
    );
    test_children.insert(
        "doc".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "cargo".to_string(),
            args: vec!["test".to_string(), "--doc".to_string()],
            ..Default::default()
        })),
    );

    project.tasks.insert(
        "tests".to_string(),
        TaskNode::Group(TaskGroup {
            type_: "group".to_string(),
            children: test_children,
            depends_on: vec![],
            description: None,
            max_concurrency: None,
        }),
    );

    // Create a task that depends on the group
    project.tasks.insert(
        "check".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "echo".to_string(),
            args: vec!["done".to_string()],
            depends_on: vec![TaskDependency::from_name("tests")],
            ..Default::default()
        })),
    );

    let compiler = Compiler::new(project);
    let ir = compiler.compile().unwrap();

    // Find the check task
    let check_task = ir.tasks.iter().find(|t| t.id == "check").unwrap();

    // Dependencies should be expanded to the leaf tasks (sorted alphabetically)
    assert_eq!(
        check_task.depends_on,
        vec!["tests.doc", "tests.unit"],
        "Group dependency should expand to leaf tasks"
    );
}

#[test]
fn test_expand_dependency_leaf_task_unchanged() {
    // Test that dependencies on leaf tasks remain unchanged
    let mut project = Project::new("test-project");

    project.tasks.insert(
        "build".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "cargo".to_string(),
            args: vec!["build".to_string()],
            ..Default::default()
        })),
    );

    project.tasks.insert(
        "test".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "cargo".to_string(),
            args: vec!["test".to_string()],
            depends_on: vec![TaskDependency::from_name("build")],
            ..Default::default()
        })),
    );

    let compiler = Compiler::new(project);
    let ir = compiler.compile().unwrap();

    let test_task = ir.tasks.iter().find(|t| t.id == "test").unwrap();
    assert_eq!(
        test_task.depends_on,
        vec!["build"],
        "Leaf task dependency should remain unchanged"
    );
}

#[test]
fn test_expand_dependency_nested_groups() {
    // Test that nested groups are recursively expanded
    let mut project = Project::new("test-project");

    // Create inner group
    let mut inner_children = HashMap::new();
    inner_children.insert(
        "a".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "echo".to_string(),
            args: vec!["a".to_string()],
            ..Default::default()
        })),
    );
    inner_children.insert(
        "b".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "echo".to_string(),
            args: vec!["b".to_string()],
            ..Default::default()
        })),
    );

    // Create outer group containing inner group
    let mut outer_children = HashMap::new();
    outer_children.insert(
        "inner".to_string(),
        TaskNode::Group(TaskGroup {
            type_: "group".to_string(),
            children: inner_children,
            depends_on: vec![],
            description: None,
            max_concurrency: None,
        }),
    );
    outer_children.insert(
        "leaf".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "echo".to_string(),
            args: vec!["leaf".to_string()],
            ..Default::default()
        })),
    );

    project.tasks.insert(
        "outer".to_string(),
        TaskNode::Group(TaskGroup {
            type_: "group".to_string(),
            children: outer_children,
            depends_on: vec![],
            description: None,
            max_concurrency: None,
        }),
    );

    project.tasks.insert(
        "final".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "echo".to_string(),
            args: vec!["final".to_string()],
            depends_on: vec![TaskDependency::from_name("outer")],
            ..Default::default()
        })),
    );

    let compiler = Compiler::new(project);
    let ir = compiler.compile().unwrap();

    let final_task = ir.tasks.iter().find(|t| t.id == "final").unwrap();
    assert_eq!(
        final_task.depends_on,
        vec!["outer.inner.a", "outer.inner.b", "outer.leaf"],
        "Nested group should be recursively expanded"
    );
}

#[test]
fn test_expand_dependency_sibling_resolution() {
    // Test that sibling task references are resolved correctly
    // This tests the case where docs.deploy depends on "build" (a sibling)
    let mut project = Project::new("test-project");

    // Create a group with two tasks: build and deploy
    // deploy depends on "build" (sibling reference, not "docs.build")
    let mut docs_children = HashMap::new();
    docs_children.insert(
        "build".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "npm".to_string(),
            args: vec!["run".to_string(), "build".to_string()],
            ..Default::default()
        })),
    );
    docs_children.insert(
        "deploy".to_string(),
        TaskNode::Task(Box::new(Task {
            command: "npm".to_string(),
            args: vec!["run".to_string(), "deploy".to_string()],
            // This simulates `dependsOn: [build]` which gets extracted as just "build"
            depends_on: vec![TaskDependency::from_name("build")],
            ..Default::default()
        })),
    );

    project.tasks.insert(
        "docs".to_string(),
        TaskNode::Group(TaskGroup {
            type_: "group".to_string(),
            children: docs_children,
            depends_on: vec![],
            description: None,
            max_concurrency: None,
        }),
    );

    let compiler = Compiler::new(project);
    let ir = compiler.compile().unwrap();

    // Find the docs.deploy task
    let deploy_task = ir.tasks.iter().find(|t| t.id == "docs.deploy").unwrap();

    // The "build" dependency should be resolved to "docs.build" (sibling)
    assert_eq!(
        deploy_task.depends_on,
        vec!["docs.build"],
        "Sibling reference 'build' should resolve to 'docs.build'"
    );
}

// =========================================================================
// Provider Detection Tests (value_has_provider / parts_have_provider)
// =========================================================================

#[test]
fn test_value_has_provider_interpolated_with_exec_secret() {
    use cuenv_core::environment::{EnvPart, EnvValue};
    use cuenv_core::secrets::Secret;

    let secret = Secret::new("echo".to_string(), vec!["test".to_string()]);
    let parts = vec![
        EnvPart::Literal("prefix-".to_string()),
        EnvPart::Secret(secret),
    ];
    let value = EnvValue::Interpolated(parts);

    // exec secret should NOT match onepassword provider
    assert!(!Compiler::value_has_provider(
        &value,
        &["onepassword".to_string()]
    ));
}

#[test]
fn test_value_has_provider_interpolated_with_onepassword_secret() {
    use cuenv_core::environment::{EnvPart, EnvValue};
    use cuenv_core::secrets::Secret;

    let secret = Secret::onepassword("op://vault/item/field");
    let parts = vec![
        EnvPart::Literal("prefix-".to_string()),
        EnvPart::Secret(secret),
    ];
    let value = EnvValue::Interpolated(parts);

    // onepassword secret SHOULD match onepassword provider
    assert!(Compiler::value_has_provider(
        &value,
        &["onepassword".to_string()]
    ));
}

#[test]
fn test_value_has_provider_with_infisical_secret() {
    use cuenv_core::environment::EnvValue;
    use cuenv_core::secrets::Secret;
    use serde_json::json;
    use std::collections::HashMap;

    let mut extra = HashMap::new();
    extra.insert("projectId".to_string(), json!("project"));
    extra.insert("environment".to_string(), json!("prod"));
    extra.insert("secretName".to_string(), json!("API_KEY"));
    let value = EnvValue::Secret(Secret {
        resolver: "infisical".to_string(),
        command: String::new(),
        args: Vec::new(),
        op_ref: None,
        extra,
    });

    assert!(Compiler::value_has_provider(
        &value,
        &["infisical".to_string()]
    ));
    assert!(!Compiler::value_has_provider(
        &value,
        &["onepassword".to_string()]
    ));
}

#[test]
fn test_value_has_provider_with_policies_infisical_secret() {
    use cuenv_core::environment::{EnvValue, EnvValueSimple, EnvVarWithPolicies};
    use cuenv_core::secrets::Secret;
    use serde_json::json;
    use std::collections::HashMap;

    let mut extra = HashMap::new();
    extra.insert("projectId".to_string(), json!("project"));
    extra.insert("environment".to_string(), json!("prod"));
    extra.insert("secretName".to_string(), json!("API_KEY"));
    let value = EnvValue::WithPolicies(EnvVarWithPolicies {
        value: EnvValueSimple::Secret(Secret {
            resolver: "infisical".to_string(),
            command: String::new(),
            args: Vec::new(),
            op_ref: None,
            extra,
        }),
        policies: None,
    });

    assert!(Compiler::value_has_provider(
        &value,
        &["infisical".to_string()]
    ));
}

#[test]
fn test_value_has_provider_interpolated_only_literals() {
    use cuenv_core::environment::{EnvPart, EnvValue};

    let parts = vec![
        EnvPart::Literal("hello".to_string()),
        EnvPart::Literal("world".to_string()),
    ];
    let value = EnvValue::Interpolated(parts);

    // No secrets = no provider match
    assert!(!Compiler::value_has_provider(
        &value,
        &["onepassword".to_string()]
    ));
}

#[test]
fn test_value_has_provider_interpolated_with_op_uri_in_literal() {
    use cuenv_core::environment::{EnvPart, EnvValue};

    // A literal string containing op:// should match onepassword provider
    let parts = vec![
        EnvPart::Literal("op://vault/item/field".to_string()),
        EnvPart::Literal("-suffix".to_string()),
    ];
    let value = EnvValue::Interpolated(parts);

    assert!(Compiler::value_has_provider(
        &value,
        &["onepassword".to_string()]
    ));
}

#[test]
fn test_value_has_provider_with_policies_interpolated() {
    use cuenv_core::environment::{EnvPart, EnvValue, EnvValueSimple, EnvVarWithPolicies};
    use cuenv_core::secrets::Secret;

    let secret = Secret::onepassword("op://vault/item/field");
    let parts = vec![
        EnvPart::Literal("prefix-".to_string()),
        EnvPart::Secret(secret),
    ];

    let value = EnvValue::WithPolicies(EnvVarWithPolicies {
        value: EnvValueSimple::Interpolated(parts),
        policies: None,
    });

    assert!(Compiler::value_has_provider(
        &value,
        &["onepassword".to_string()]
    ));
}

#[test]
fn test_parts_have_provider_op_uri_in_literal() {
    use cuenv_core::environment::EnvPart;

    let parts = vec![
        EnvPart::Literal("prefix-".to_string()),
        EnvPart::Literal("op://vault/item/password".to_string()),
    ];

    // op:// URI in literal should match onepassword
    assert!(Compiler::parts_have_provider(
        &parts,
        &["onepassword".to_string()]
    ));
}

#[test]
fn test_parts_have_provider_op_uri_not_matching_other_providers() {
    use cuenv_core::environment::EnvPart;

    let parts = vec![EnvPart::Literal("op://vault/item/password".to_string())];

    // op:// should NOT match other providers like "aws" or "vault"
    assert!(!Compiler::parts_have_provider(&parts, &["aws".to_string()]));
    assert!(!Compiler::parts_have_provider(
        &parts,
        &["vault".to_string()]
    ));
}
