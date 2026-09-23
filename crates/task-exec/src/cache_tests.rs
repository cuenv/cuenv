use super::*;
use crate::{Input, Task, TaskCacheMode, TaskCachePolicy};
use cuenv_cas::{LocalActionCache, LocalCas};
use cuenv_core::environment::Environment;
use cuenv_vcs::WalkHasher;
use std::fs;
use tempfile::TempDir;

fn make_cache(root: &Path) -> TaskCacheConfig {
    TaskCacheConfig {
        cas: Arc::new(LocalCas::open(root).unwrap()),
        action_cache: Arc::new(LocalActionCache::open(root).unwrap()),
        vcs_hasher: Arc::new(WalkHasher::new(root)),
        vcs_hasher_root: root.to_path_buf(),
        action_semantics_version: 1,
        runtime_identity_properties: BTreeMap::new(),
        cache_disabled_reason: None,
        secret_salt: Some("test-salt".to_string()),
        mode_override: None,
        cache_root: root.to_path_buf(),
        project_roots: BTreeMap::new(),
    }
}

fn make_task(command: &str, args: &[&str], inputs: &[&str], outputs: &[&str]) -> Task {
    Task {
        command: command.to_string(),
        args: args.iter().map(|arg| (*arg).to_string()).collect(),
        inputs: inputs
            .iter()
            .map(|path| Input::Path((*path).to_string()))
            .collect(),
        outputs: outputs.iter().map(|output| (*output).to_string()).collect(),
        cache: Some(TaskCachePolicy {
            mode: TaskCacheMode::ReadWrite,
            max_age: None,
        }),
        ..Task::default()
    }
}

async fn build_action_for_test(input: BuildActionInput<'_>) -> Option<(Action, Digest)> {
    match build_action(input).await.unwrap() {
        CacheOutcome::Eligible(eligible) => Some((eligible.action, eligible.digest)),
        CacheOutcome::Skipped { .. } => None,
    }
}

#[tokio::test]
async fn build_action_returns_none_when_no_inputs() {
    let tmp = TempDir::new().unwrap();
    let cache = make_cache(tmp.path());
    let task = make_task("echo", &["hi"], &[], &[]);
    let env = Environment::new();

    let result = build_action_for_test(BuildActionInput {
        task: &task,
        task_name: "no-inputs",
        environment: &env,
        cache: &cache,
        workdir: tmp.path(),
        project_root: tmp.path(),
    })
    .await;
    assert!(result.is_none());
}

#[tokio::test]
async fn build_action_is_deterministic() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("input.txt"), "payload").unwrap();
    let cache = make_cache(tmp.path());
    let task = make_task("echo", &["hi"], &["input.txt"], &[]);
    let env = Environment::new();

    let (_, first) = build_action_for_test(BuildActionInput {
        task: &task,
        task_name: "t",
        environment: &env,
        cache: &cache,
        workdir: tmp.path(),
        project_root: tmp.path(),
    })
    .await
    .unwrap();
    let (_, second) = build_action_for_test(BuildActionInput {
        task: &task,
        task_name: "t",
        environment: &env,
        cache: &cache,
        workdir: tmp.path(),
        project_root: tmp.path(),
    })
    .await
    .unwrap();
    assert_eq!(first, second);
}

#[tokio::test]
async fn build_action_changes_when_input_changes() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("input.txt"), "first").unwrap();
    let cache = make_cache(tmp.path());
    let task = make_task("echo", &["hi"], &["input.txt"], &[]);
    let env = Environment::new();

    let (_, first) = build_action_for_test(BuildActionInput {
        task: &task,
        task_name: "t",
        environment: &env,
        cache: &cache,
        workdir: tmp.path(),
        project_root: tmp.path(),
    })
    .await
    .unwrap();

    fs::write(tmp.path().join("input.txt"), "second").unwrap();
    let (_, second) = build_action_for_test(BuildActionInput {
        task: &task,
        task_name: "t",
        environment: &env,
        cache: &cache,
        workdir: tmp.path(),
        project_root: tmp.path(),
    })
    .await
    .unwrap();

    assert_ne!(first, second);
}

#[tokio::test]
async fn build_action_changes_when_timeout_changes() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("input.txt"), "payload").unwrap();
    let cache = make_cache(tmp.path());
    let mut task = make_task("echo", &["hi"], &["input.txt"], &[]);
    let env = Environment::new();

    task.timeout = Some("10s".to_string());
    let (_, first) = build_action_for_test(BuildActionInput {
        task: &task,
        task_name: "t",
        environment: &env,
        cache: &cache,
        workdir: tmp.path(),
        project_root: tmp.path(),
    })
    .await
    .unwrap();

    task.timeout = Some("1s".to_string());
    let (_, second) = build_action_for_test(BuildActionInput {
        task: &task,
        task_name: "t",
        environment: &env,
        cache: &cache,
        workdir: tmp.path(),
        project_root: tmp.path(),
    })
    .await
    .unwrap();

    assert_ne!(first, second);
}

#[tokio::test]
async fn build_action_returns_none_when_task_has_task_level_env() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("input.txt"), "payload").unwrap();
    let side_effect = tmp.path().join("side-effect.txt");
    let cache = make_cache(tmp.path());
    let mut task = make_task("echo", &["hi"], &["input.txt"], &[]);
    task.env.insert(
        "GH_TOKEN".to_string(),
        serde_json::json!({
            "resolver": "exec",
            "command": "sh",
            "args": [
                "-c",
                format!("echo touched > {}", side_effect.display())
            ]
        }),
    );
    let env = Environment::new();

    let result = build_action_for_test(BuildActionInput {
        task: &task,
        task_name: "task-env",
        environment: &env,
        cache: &cache,
        workdir: tmp.path(),
        project_root: tmp.path(),
    })
    .await;

    assert!(result.is_none());
    assert!(!side_effect.exists());
}

#[tokio::test]
async fn runtime_env_skip_keeps_resolved_inputs_for_sandboxing() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("input.txt"), "payload").unwrap();
    let cache = make_cache(tmp.path());

    let mut task = make_task("echo", &["hi"], &["input.txt"], &[]);
    task.env
        .insert("TOKEN".to_string(), serde_json::json!("runtime"));
    let env = Environment::new();

    let outcome = build_action(BuildActionInput {
        task: &task,
        task_name: "task-env",
        environment: &env,
        cache: &cache,
        workdir: tmp.path(),
        project_root: tmp.path(),
    })
    .await
    .unwrap();

    let CacheOutcome::Skipped { reason, execution } = outcome else {
        panic!("task-level runtime environment must skip result caching");
    };
    assert_eq!(reason, CacheSkipReason::RuntimeEnv);
    assert_eq!(execution.unwrap().inputs.len(), 1);
}

#[tokio::test]
async fn input_resolution_failure_precedes_runtime_env_cache_skip() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("input.txt"), "payload").unwrap();
    let mut cache = make_cache(tmp.path());
    cache.vcs_hasher_root = tmp.path().join("other-root");

    let mut task = make_task("echo", &["hi"], &["input.txt"], &[]);
    task.env
        .insert("TOKEN".to_string(), serde_json::json!("runtime"));
    let env = Environment::new();

    let outcome = build_action(BuildActionInput {
        task: &task,
        task_name: "task-env",
        environment: &env,
        cache: &cache,
        workdir: tmp.path(),
        project_root: tmp.path(),
    })
    .await
    .unwrap();

    assert!(matches!(
        outcome,
        CacheOutcome::Skipped {
            reason: CacheSkipReason::HasherRootMismatch,
            ..
        }
    ));
}

#[tokio::test]
async fn build_action_hashes_inputs_relative_to_task_project_root() {
    let tmp = TempDir::new().unwrap();
    let workspace_root = tmp.path();
    let nested_project_root = workspace_root.join("packages/app");
    fs::create_dir_all(nested_project_root.join("src")).unwrap();
    fs::create_dir_all(workspace_root.join("src")).unwrap();
    fs::write(workspace_root.join("src/input.txt"), "workspace-root").unwrap();
    fs::write(nested_project_root.join("src/input.txt"), "nested-project").unwrap();

    let cache = make_cache(workspace_root);
    let task = make_task("echo", &["hi"], &["src/input.txt"], &[]);
    let env = Environment::new();

    let (_, first) = build_action_for_test(BuildActionInput {
        task: &task,
        task_name: "nested",
        environment: &env,
        cache: &cache,
        workdir: &nested_project_root,
        project_root: &nested_project_root,
    })
    .await
    .unwrap();

    fs::write(
        workspace_root.join("src/input.txt"),
        "workspace-root-updated",
    )
    .unwrap();
    let (_, second) = build_action_for_test(BuildActionInput {
        task: &task,
        task_name: "nested",
        environment: &env,
        cache: &cache,
        workdir: &nested_project_root,
        project_root: &nested_project_root,
    })
    .await
    .unwrap();

    assert_eq!(first, second);

    fs::write(
        nested_project_root.join("src/input.txt"),
        "nested-project-updated",
    )
    .unwrap();
    let (_, third) = build_action_for_test(BuildActionInput {
        task: &task,
        task_name: "nested",
        environment: &env,
        cache: &cache,
        workdir: &nested_project_root,
        project_root: &nested_project_root,
    })
    .await
    .unwrap();

    assert_ne!(first, third);
}

#[tokio::test]
async fn build_action_changes_when_command_changes() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("input.txt"), "payload").unwrap();
    let cache = make_cache(tmp.path());
    let env = Environment::new();

    let task1 = make_task("cargo", &["build"], &["input.txt"], &[]);
    let task2 = make_task("cargo", &["test"], &["input.txt"], &[]);

    let (_, first) = build_action_for_test(BuildActionInput {
        task: &task1,
        task_name: "t",
        environment: &env,
        cache: &cache,
        workdir: tmp.path(),
        project_root: tmp.path(),
    })
    .await
    .unwrap();
    let (_, second) = build_action_for_test(BuildActionInput {
        task: &task2,
        task_name: "t",
        environment: &env,
        cache: &cache,
        workdir: tmp.path(),
        project_root: tmp.path(),
    })
    .await
    .unwrap();
    assert_ne!(first, second);
}

#[tokio::test]
async fn build_action_changes_when_script_changes() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("input.txt"), "payload").unwrap();
    let cache = make_cache(tmp.path());
    let env = Environment::new();

    let task1 = Task {
        script: Some("echo one".to_string()),
        inputs: vec![Input::Path("input.txt".to_string())],
        cache: Some(TaskCachePolicy {
            mode: TaskCacheMode::ReadWrite,
            max_age: None,
        }),
        ..Task::default()
    };
    let task2 = Task {
        script: Some("echo two".to_string()),
        inputs: vec![Input::Path("input.txt".to_string())],
        cache: Some(TaskCachePolicy {
            mode: TaskCacheMode::ReadWrite,
            max_age: None,
        }),
        ..Task::default()
    };

    let (_, first) = build_action_for_test(BuildActionInput {
        task: &task1,
        task_name: "script",
        environment: &env,
        cache: &cache,
        workdir: tmp.path(),
        project_root: tmp.path(),
    })
    .await
    .unwrap();
    let (_, second) = build_action_for_test(BuildActionInput {
        task: &task2,
        task_name: "script",
        environment: &env,
        cache: &cache,
        workdir: tmp.path(),
        project_root: tmp.path(),
    })
    .await
    .unwrap();

    assert_ne!(first, second);
}

#[tokio::test]
async fn sandbox_tier_changes_action_digest() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("input.txt"), "same").unwrap();
    let cache = make_cache(tmp.path());
    let env = Environment::new();
    let isolated = make_task("echo", &["hi"], &["input.txt"], &["out.txt"]);
    let mut unisolated = isolated.clone();
    unisolated.hermetic = crate::Hermetic::Options(crate::HermeticOptions {
        passthrough: Vec::new(),
        sandbox: Some(crate::Sandbox::None),
    });

    let (_, isolated_digest) = build_action_for_test(BuildActionInput {
        task: &isolated,
        task_name: "sandbox",
        environment: &env,
        cache: &cache,
        workdir: tmp.path(),
        project_root: tmp.path(),
    })
    .await
    .unwrap();
    let (_, unisolated_digest) = build_action_for_test(BuildActionInput {
        task: &unisolated,
        task_name: "sandbox",
        environment: &env,
        cache: &cache,
        workdir: tmp.path(),
        project_root: tmp.path(),
    })
    .await
    .unwrap();

    assert_ne!(isolated_digest, unisolated_digest);
}

#[tokio::test]
async fn record_then_lookup_roundtrips() {
    let tmp = TempDir::new().unwrap();
    let workdir = tmp.path().join("work");
    fs::create_dir_all(&workdir).unwrap();
    fs::write(tmp.path().join("input.txt"), "in").unwrap();
    fs::write(workdir.join("out.txt"), "produced").unwrap();

    let cache = make_cache(tmp.path());
    let task = make_task("echo", &["hi"], &["input.txt"], &["out.txt"]);
    let env = Environment::new();

    let (_, action_digest) = build_action_for_test(BuildActionInput {
        task: &task,
        task_name: "t",
        environment: &env,
        cache: &cache,
        workdir: &workdir,
        project_root: tmp.path(),
    })
    .await
    .unwrap();

    record(RecordInput {
        cache: &cache,
        action_digest: &action_digest,
        workdir: &workdir,
        task: &task,
        stdout: "stdout-text",
        stderr: "stderr-text",
        exit_code: 0,
        duration_ms: 42,
    })
    .await
    .unwrap();

    let recorded = lookup(&cache, &action_digest, &task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(recorded.exit_code, 0);
    assert_eq!(recorded.output_files.len(), 1);
    assert_eq!(recorded.output_files[0].path, "out.txt");

    let fresh = tmp.path().join("fresh");
    fs::create_dir_all(&fresh).unwrap();
    let (stdout, stderr, exit_code) = materialize_hit(&cache, &fresh, &task, &recorded)
        .await
        .unwrap();
    assert_eq!(stdout, "stdout-text");
    assert_eq!(stderr, "stderr-text");
    assert_eq!(exit_code, 0);
    assert_eq!(fs::read(fresh.join("out.txt")).unwrap(), b"produced");
}

#[tokio::test]
async fn output_directory_roundtrips_as_a_reapi_tree() {
    let tmp = TempDir::new().unwrap();
    let workdir = tmp.path().join("work");
    fs::create_dir_all(workdir.join("dist/empty")).unwrap();
    fs::create_dir_all(workdir.join("dist/nested")).unwrap();
    fs::write(workdir.join("dist/nested/app.js"), "built").unwrap();

    let cache = make_cache(tmp.path());
    let task = make_task("echo", &["hi"], &["input.txt"], &["dist"]);
    let action_digest = Digest::of_bytes(b"directory-action");
    record(RecordInput {
        cache: &cache,
        action_digest: &action_digest,
        workdir: &workdir,
        task: &task,
        stdout: "",
        stderr: "",
        exit_code: 0,
        duration_ms: 1,
    })
    .await
    .unwrap();

    let recorded = lookup(&cache, &action_digest, &task)
        .await
        .unwrap()
        .unwrap();
    assert!(recorded.output_files.is_empty());
    assert_eq!(recorded.output_directories.len(), 1);

    let fresh = tmp.path().join("fresh");
    fs::create_dir_all(&fresh).unwrap();
    fs::write(fresh.join("dist"), "stale file").unwrap();
    materialize_hit(&cache, &fresh, &task, &recorded)
        .await
        .unwrap();

    assert_eq!(
        fs::read_to_string(fresh.join("dist/nested/app.js")).unwrap(),
        "built"
    );
    assert!(fresh.join("dist/empty").is_dir());
}

#[cfg(unix)]
#[tokio::test]
async fn record_and_materialize_preserve_executable_outputs() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().unwrap();
    let workdir = tmp.path().join("work");
    fs::create_dir_all(&workdir).unwrap();
    fs::write(tmp.path().join("input.txt"), "in").unwrap();
    let script = workdir.join("bin/run.sh");
    fs::create_dir_all(script.parent().unwrap()).unwrap();
    fs::write(&script, "#!/bin/sh\necho hi\n").unwrap();
    let mut permissions = fs::metadata(&script).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&script, permissions).unwrap();

    let cache = make_cache(tmp.path());
    let task = make_task("echo", &["hi"], &["input.txt"], &["bin"]);
    let env = Environment::new();

    let (_, action_digest) = build_action_for_test(BuildActionInput {
        task: &task,
        task_name: "exec",
        environment: &env,
        cache: &cache,
        workdir: &workdir,
        project_root: tmp.path(),
    })
    .await
    .unwrap();

    record(RecordInput {
        cache: &cache,
        action_digest: &action_digest,
        workdir: &workdir,
        task: &task,
        stdout: "",
        stderr: "",
        exit_code: 0,
        duration_ms: 1,
    })
    .await
    .unwrap();

    let recorded = lookup(&cache, &action_digest, &task)
        .await
        .unwrap()
        .unwrap();
    let fresh = tmp.path().join("fresh");
    fs::create_dir_all(&fresh).unwrap();
    materialize_hit(&cache, &fresh, &task, &recorded)
        .await
        .unwrap();

    let mode = fs::metadata(fresh.join("bin/run.sh"))
        .unwrap()
        .permissions()
        .mode();
    assert_ne!(mode & 0o111, 0);
}

#[tokio::test]
async fn build_action_returns_none_when_cache_mode_never() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("input.txt"), "payload").unwrap();
    let cache = make_cache(tmp.path());
    let task = Task {
        command: "echo".to_string(),
        args: vec!["hi".to_string()],
        inputs: vec![Input::Path("input.txt".to_string())],
        cache: Some(TaskCachePolicy {
            mode: TaskCacheMode::Never,
            max_age: None,
        }),
        ..Task::default()
    };
    let env = Environment::new();

    let result = build_action_for_test(BuildActionInput {
        task: &task,
        task_name: "never",
        environment: &env,
        cache: &cache,
        workdir: tmp.path(),
        project_root: tmp.path(),
    })
    .await;
    assert!(result.is_none());
}

#[tokio::test]
async fn build_action_returns_none_when_explicit_input_is_missing() {
    let tmp = TempDir::new().unwrap();
    let cache = make_cache(tmp.path());
    let task = make_task("echo", &["hi"], &["missing.txt"], &[]);
    let env = Environment::new();

    let result = build_action_for_test(BuildActionInput {
        task: &task,
        task_name: "missing",
        environment: &env,
        cache: &cache,
        workdir: tmp.path(),
        project_root: tmp.path(),
    })
    .await;

    assert!(result.is_none());
}

#[tokio::test]
async fn lookup_respects_max_age() {
    let tmp = TempDir::new().unwrap();
    let workdir = tmp.path().join("work");
    fs::create_dir_all(&workdir).unwrap();
    fs::write(tmp.path().join("input.txt"), "in").unwrap();
    fs::write(workdir.join("out.txt"), "produced").unwrap();

    let cache = make_cache(tmp.path());
    let task = Task {
        command: "echo".to_string(),
        args: vec!["hi".to_string()],
        inputs: vec![Input::Path("input.txt".to_string())],
        outputs: vec!["out.txt".to_string()],
        cache: Some(TaskCachePolicy {
            mode: TaskCacheMode::ReadWrite,
            max_age: Some("1ms".to_string()),
        }),
        ..Task::default()
    };
    let env = Environment::new();

    let (_, action_digest) = build_action_for_test(BuildActionInput {
        task: &task,
        task_name: "ttl",
        environment: &env,
        cache: &cache,
        workdir: &workdir,
        project_root: tmp.path(),
    })
    .await
    .unwrap();
    record(RecordInput {
        cache: &cache,
        action_digest: &action_digest,
        workdir: &workdir,
        task: &task,
        stdout: "stdout-text",
        stderr: "stderr-text",
        exit_code: 0,
        duration_ms: 42,
    })
    .await
    .unwrap();

    std::thread::sleep(std::time::Duration::from_millis(5));
    let lookup_result = lookup(&cache, &action_digest, &task).await.unwrap();
    assert!(lookup_result.is_none());
}

#[tokio::test]
async fn record_skips_non_zero_exit_codes() {
    let tmp = TempDir::new().unwrap();
    let workdir = tmp.path().join("work");
    fs::create_dir_all(&workdir).unwrap();
    fs::write(tmp.path().join("input.txt"), "in").unwrap();
    fs::write(workdir.join("out.txt"), "produced").unwrap();

    let cache = make_cache(tmp.path());
    let task = make_task("echo", &["hi"], &["input.txt"], &["out.txt"]);
    let env = Environment::new();

    let (_, action_digest) = build_action_for_test(BuildActionInput {
        task: &task,
        task_name: "non-zero",
        environment: &env,
        cache: &cache,
        workdir: &workdir,
        project_root: tmp.path(),
    })
    .await
    .unwrap();

    record(RecordInput {
        cache: &cache,
        action_digest: &action_digest,
        workdir: &workdir,
        task: &task,
        stdout: "stdout-text",
        stderr: "stderr-text",
        exit_code: 1,
        duration_ms: 42,
    })
    .await
    .unwrap();

    let lookup_result = lookup(&cache, &action_digest, &task).await.unwrap();
    assert!(lookup_result.is_none());
}

// =============================================================================
// Action key portability
// =============================================================================

/// Decode a stored `Command` blob from its REAPI protobuf encoding.
fn decode_command(bytes: &[u8]) -> cuenv_cas::Command {
    use bazel_remote_apis::build::bazel::remote::execution::v2 as pb;
    let proto = <pb::Command as prost::Message>::decode(bytes).expect("decode Command");
    cuenv_cas::Command::from_proto(&proto)
}

/// Decode a stored `Action` blob from its REAPI protobuf encoding.
fn decode_action(bytes: &[u8]) -> Action {
    use bazel_remote_apis::build::bazel::remote::execution::v2 as pb;
    let proto = <pb::Action as prost::Message>::decode(bytes).expect("decode Action");
    Action::from_proto(&proto).expect("convert Action")
}

async fn skip_reason_for_test(input: BuildActionInput<'_>) -> Option<CacheSkipReason> {
    match build_action(input).await.unwrap() {
        CacheOutcome::Eligible(_) => None,
        CacheOutcome::Skipped { reason, .. } => Some(reason),
    }
}

#[tokio::test]
async fn build_action_skips_non_hermetic_tasks() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("input.txt"), "payload").unwrap();
    let cache = make_cache(tmp.path());
    let mut task = make_task("echo", &["hi"], &["input.txt"], &[]);
    task.hermetic = cuenv_manifest::tasks::Hermetic::Enabled(false);
    let env = Environment::new();

    let reason = skip_reason_for_test(BuildActionInput {
        task: &task,
        task_name: "non-hermetic",
        environment: &env,
        cache: &cache,
        workdir: tmp.path(),
        project_root: tmp.path(),
    })
    .await;

    assert_eq!(reason, Some(CacheSkipReason::NonHermetic));
}

#[tokio::test]
async fn build_action_skips_workdir_outside_project_and_module_roots() {
    let tmp = TempDir::new().unwrap();
    let outside = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    let elsewhere = outside.path().join("elsewhere");
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(&elsewhere).unwrap();
    fs::write(project.join("input.txt"), "payload").unwrap();

    let cache = make_cache(tmp.path());
    let task = make_task("echo", &["hi"], &["input.txt"], &[]);
    let env = Environment::new();

    let reason = skip_reason_for_test(BuildActionInput {
        task: &task,
        task_name: "stray-workdir",
        environment: &env,
        cache: &cache,
        // Outside the workspace: normalizing would bake an absolute host
        // path into the key.
        workdir: &elsewhere,
        project_root: &project,
    })
    .await;

    assert_eq!(reason, Some(CacheSkipReason::UnportableWorkdir));
}

#[tokio::test]
async fn action_environment_is_declared_only() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("input.txt"), "payload").unwrap();
    let cache = make_cache(tmp.path());
    let task = make_task("echo", &["hi"], &["input.txt"], &[]);
    let mut env = Environment::new();
    env.set("DECLARED".to_string(), "yes".to_string());

    let (action, _) = build_action_for_test(BuildActionInput {
        task: &task,
        task_name: "declared-env",
        environment: &env,
        cache: &cache,
        workdir: tmp.path(),
        project_root: tmp.path(),
    })
    .await
    .unwrap();

    // Re-decode the stored Command blob: it is what the digest was taken over,
    // and it is REAPI protobuf, exactly as a remote server would store it.
    let bytes = cache.cas.get(&action.command_digest).await.unwrap();
    let command = decode_command(&bytes);

    assert_eq!(
        command
            .environment_variables
            .get("DECLARED")
            .map(String::as_str),
        Some("yes")
    );
    for ambient in ["HOME", "USER", "TERM", "XDG_CACHE_HOME"] {
        assert!(
            !command.environment_variables.contains_key(ambient),
            "{ambient} leaked into the action key"
        );
    }
}

#[tokio::test]
async fn build_action_stores_action_and_command_blobs() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("input.txt"), "payload").unwrap();
    let cache = make_cache(tmp.path());
    let task = make_task("echo", &["hi"], &["input.txt"], &[]);
    let env = Environment::new();

    let (action, action_digest) = build_action_for_test(BuildActionInput {
        task: &task,
        task_name: "stored",
        environment: &env,
        cache: &cache,
        workdir: tmp.path(),
        project_root: tmp.path(),
    })
    .await
    .unwrap();

    // Both blobs present means a later `explain` can diff two keys instead of
    // just reporting that they differ.
    assert!(cache.cas.contains(&action_digest).await.unwrap());
    assert!(cache.cas.contains(&action.command_digest).await.unwrap());

    let stored = decode_action(&cache.cas.get(&action_digest).await.unwrap());
    assert_eq!(stored, action);
}

// =============================================================================
// Cache-hit integrity
// =============================================================================

#[tokio::test]
async fn lookup_ignores_an_entry_whose_output_blob_was_evicted() {
    let tmp = TempDir::new().unwrap();
    let workdir = tmp.path().join("work");
    fs::create_dir_all(&workdir).unwrap();
    fs::write(workdir.join("out.txt"), "produced").unwrap();

    let cache = make_cache(tmp.path());
    let task = make_task("echo", &["hi"], &["out.txt"], &["out.txt"]);
    let action_digest = Digest::of_bytes(b"evicted-action");

    record(RecordInput {
        cache: &cache,
        action_digest: &action_digest,
        workdir: &workdir,
        task: &task,
        stdout: "",
        stderr: "",
        exit_code: 0,
        duration_ms: 1,
    })
    .await
    .unwrap();
    assert!(
        lookup(&cache, &action_digest, &task)
            .await
            .unwrap()
            .is_some()
    );

    // Simulate garbage collection removing the output blob.
    let stored = cache
        .action_cache
        .lookup(&action_digest)
        .await
        .unwrap()
        .unwrap();
    let blob = tmp.path().join("cas").join("sha256").join(
        Path::new(&stored.output_files[0].digest.hash[..2])
            .join(&stored.output_files[0].digest.hash[2..]),
    );
    fs::remove_file(&blob).unwrap();

    assert!(
        lookup(&cache, &action_digest, &task)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        fs::read_to_string(workdir.join("out.txt")).unwrap(),
        "produced"
    );
}

#[tokio::test]
async fn materialize_hit_rejects_output_paths_that_escape_the_workdir() {
    let tmp = TempDir::new().unwrap();
    let workdir = tmp.path().join("work");
    fs::create_dir_all(&workdir).unwrap();
    let cache = make_cache(tmp.path());

    let digest = cache.cas.put_bytes(b"owned").await.unwrap();
    let result = ActionResult {
        output_files: vec![OutputFile {
            path: "../escaped.txt".to_string(),
            digest,
            is_executable: false,
        }],
        output_directories: vec![],
        exit_code: 0,
        stdout_digest: None,
        stderr_digest: None,
        execution_metadata: ExecutionMetadata::default(),
    };
    let task = make_task("echo", &[], &[], &["escaped.txt"]);

    let error = materialize_hit(&cache, &workdir, &task, &result)
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("stay inside the working directory"),
        "unexpected error: {error}"
    );
    assert!(!tmp.path().join("escaped.txt").exists());
}

#[tokio::test]
async fn materialize_hit_leaves_existing_outputs_intact_when_a_blob_is_missing() {
    let tmp = TempDir::new().unwrap();
    let workdir = tmp.path().join("work");
    fs::create_dir_all(&workdir).unwrap();
    fs::write(workdir.join("first.txt"), "original first").unwrap();
    fs::write(workdir.join("second.txt"), "original second").unwrap();

    let cache = make_cache(tmp.path());
    let present = cache.cas.put_bytes(b"cached first").await.unwrap();

    let result = ActionResult {
        output_files: vec![
            OutputFile {
                path: "first.txt".to_string(),
                digest: present,
                is_executable: false,
            },
            OutputFile {
                path: "second.txt".to_string(),
                digest: Digest::of_bytes(b"never stored"),
                is_executable: false,
            },
        ],
        output_directories: vec![],
        exit_code: 0,
        stdout_digest: None,
        stderr_digest: None,
        execution_metadata: ExecutionMetadata::default(),
    };
    let task = make_task("echo", &[], &[], &["first.txt", "second.txt"]);

    assert!(
        materialize_hit(&cache, &workdir, &task, &result)
            .await
            .is_err()
    );

    // Staging means the first output is never installed, so the workspace is
    // not left as a mix of cached and pre-existing files.
    assert_eq!(
        fs::read_to_string(workdir.join("first.txt")).unwrap(),
        "original first"
    );
    assert_eq!(
        fs::read_to_string(workdir.join("second.txt")).unwrap(),
        "original second"
    );
}

#[tokio::test]
async fn materialize_hit_removes_its_staging_directory() {
    let tmp = TempDir::new().unwrap();
    let workdir = tmp.path().join("work");
    fs::create_dir_all(&workdir).unwrap();
    let cache = make_cache(tmp.path());

    let digest = cache.cas.put_bytes(b"restored").await.unwrap();
    let result = ActionResult {
        output_files: vec![OutputFile {
            path: "nested/out.txt".to_string(),
            digest,
            is_executable: false,
        }],
        output_directories: vec![],
        exit_code: 0,
        stdout_digest: None,
        stderr_digest: None,
        execution_metadata: ExecutionMetadata::default(),
    };
    let task = make_task("echo", &[], &[], &["nested/out.txt"]);

    materialize_hit(&cache, &workdir, &task, &result)
        .await
        .unwrap();

    assert_eq!(
        fs::read_to_string(workdir.join("nested/out.txt")).unwrap(),
        "restored"
    );
    let leftovers: Vec<_> = fs::read_dir(&workdir)
        .unwrap()
        .filter_map(std::result::Result::ok)
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with(cuenv_vcs::SCRATCH_PREFIX)
        })
        .collect();
    assert!(leftovers.is_empty(), "staging directory was left behind");
}

#[test]
fn cached_result_rejects_ancestor_output_overlaps() {
    let task = make_task("echo", &[], &[], &["dist"]);
    let result = ActionResult {
        output_files: vec![OutputFile {
            path: "dist/app.js".to_string(),
            digest: Digest::of_bytes(b"app"),
            is_executable: false,
        }],
        output_directories: vec![cuenv_cas::OutputDirectory {
            path: "dist".to_string(),
            tree_digest: Digest::of_bytes(b"tree"),
        }],
        ..ActionResult::default()
    };

    let error = validate_cached_result(&task, &result).unwrap_err();
    assert!(error.to_string().contains("overlapping output path"));
}

#[test]
fn output_globs_skip_scratch_directories() {
    // Another task projecting into the same workdir, or a run that was
    // killed mid-projection, leaves a scratch copy here. It is not ours.
    let tmp = TempDir::new().unwrap();
    let workdir = tmp.path();
    fs::create_dir_all(workdir.join("dist")).unwrap();
    fs::write(workdir.join("dist/app.js"), "app").unwrap();
    let scratch = workdir.join(format!("{}1-0", cuenv_vcs::SCRATCH_PREFIX));
    fs::create_dir_all(scratch.join("dist")).unwrap();
    fs::write(scratch.join("dist/other.js"), "other").unwrap();

    let outputs = collect_outputs(workdir, &["**/*.js".to_string()]).unwrap();

    assert_eq!(outputs, vec![PathBuf::from("dist/app.js")]);
}

#[tokio::test]
async fn an_invalid_cached_result_is_a_miss_not_a_failure() {
    // A shared cache can hand back an entry this task would never have
    // written. That must cost a rerun, not fail the task on every run.
    let tmp = TempDir::new().unwrap();
    let cache = make_cache(tmp.path());
    let task = make_task("echo", &[], &[], &["dist"]);
    let action_digest = Digest::of_bytes(b"some-action");
    let result = ActionResult {
        output_files: vec![OutputFile {
            path: "elsewhere/secret.txt".to_string(),
            digest: Digest::of_bytes(b"secret"),
            is_executable: false,
        }],
        ..ActionResult::default()
    };
    cache
        .action_cache
        .update(&action_digest, &result)
        .await
        .unwrap();

    assert!(
        lookup(&cache, &action_digest, &task)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn cache_hit_removes_a_declared_output_absent_from_the_result() {
    let tmp = TempDir::new().unwrap();
    let workdir = tmp.path().join("work");
    fs::create_dir_all(&workdir).unwrap();
    fs::write(workdir.join("stale.txt"), "old").unwrap();
    let cache = make_cache(tmp.path());
    let task = make_task("echo", &[], &[], &["stale.txt"]);
    let result = ActionResult {
        exit_code: 0,
        ..ActionResult::default()
    };

    materialize_hit(&cache, &workdir, &task, &result)
        .await
        .unwrap();

    assert!(!workdir.join("stale.txt").exists());
}

#[tokio::test]
async fn a_resolved_secret_is_never_written_into_the_stored_command_blob() {
    // `store_message` puts the Command message in the CAS at key-computation
    // time. Before secrets were classified, a resolved credential landed
    // there in plaintext — and a remote cache would have uploaded it.
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("input.txt"), "payload").unwrap();
    let cache = make_cache(tmp.path());
    let task = make_task("echo", &["hi"], &["input.txt"], &[]);

    let mut env = Environment::new();
    env.set_secret("API_KEY".to_string(), "hunter2".to_string());

    let (action, _) = build_action_for_test(BuildActionInput {
        task: &task,
        task_name: "secret-env",
        environment: &env,
        cache: &cache,
        workdir: tmp.path(),
        project_root: tmp.path(),
    })
    .await
    .unwrap();

    let bytes = cache.cas.get(&action.command_digest).await.unwrap();
    assert!(
        !String::from_utf8_lossy(&bytes).contains("hunter2"),
        "the secret reached the content-addressed store"
    );

    let command = decode_command(&bytes);
    let recorded = command.environment_variables.get("API_KEY").unwrap();
    assert!(recorded.starts_with("cuenv-secret-fp:"), "{recorded}");
}

#[tokio::test]
async fn a_task_with_secrets_and_no_salt_is_not_cached() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("input.txt"), "payload").unwrap();
    let mut cache = make_cache(tmp.path());
    cache.secret_salt = None;
    let task = make_task("echo", &["hi"], &["input.txt"], &[]);

    let mut env = Environment::new();
    env.set_secret("API_KEY".to_string(), "hunter2".to_string());

    let reason = skip_reason_for_test(BuildActionInput {
        task: &task,
        task_name: "secret-env",
        environment: &env,
        cache: &cache,
        workdir: tmp.path(),
        project_root: tmp.path(),
    })
    .await;

    assert_eq!(reason, Some(CacheSkipReason::SecretsWithoutCacheSalt));
}

#[tokio::test]
async fn rotating_a_secret_changes_the_action_digest() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("input.txt"), "payload").unwrap();
    let cache = make_cache(tmp.path());
    let task = make_task("echo", &["hi"], &["input.txt"], &[]);

    let digest_for = |value: &str| {
        let mut env = Environment::new();
        env.set_secret("API_KEY".to_string(), value.to_string());
        let cache = &cache;
        let task = &task;
        let tmp = &tmp;
        async move {
            build_action_for_test(BuildActionInput {
                task,
                task_name: "secret-env",
                environment: &env,
                cache,
                workdir: tmp.path(),
                project_root: tmp.path(),
            })
            .await
            .unwrap()
            .1
        }
    };

    // A revoked credential must not keep serving results produced with it.
    assert_ne!(digest_for("old").await, digest_for("new").await);
}

// =============================================================================
// Output collection scope
// =============================================================================

#[test]
fn output_walk_roots_bound_a_literal_pattern() {
    let workdir = Path::new("/w");
    let roots = output_walk_roots(workdir, &["target/release/app".to_string()]);
    assert_eq!(roots, vec![PathBuf::from("/w/target/release/app")]);
}

#[test]
fn output_walk_roots_stop_at_the_first_wildcard() {
    let workdir = Path::new("/w");
    let roots = output_walk_roots(workdir, &["dist/**/*.js".to_string()]);
    assert_eq!(roots, vec![PathBuf::from("/w/dist")]);
}

#[test]
fn a_leading_wildcard_forces_the_whole_workdir() {
    let workdir = Path::new("/w");
    let roots = output_walk_roots(workdir, &["**/*.js".to_string(), "dist/app".to_string()]);
    assert_eq!(roots, vec![PathBuf::from("/w")]);
}

#[test]
fn nested_roots_are_collapsed_so_no_subtree_is_walked_twice() {
    let workdir = Path::new("/w");
    let roots = output_walk_roots(
        workdir,
        &[
            "dist/nested/deep/**/*".to_string(),
            "dist/**/*".to_string(),
            "other/x".to_string(),
        ],
    );
    assert_eq!(
        roots,
        vec![PathBuf::from("/w/dist"), PathBuf::from("/w/other/x")]
    );
}

#[tokio::test]
async fn output_collection_ignores_trees_outside_the_declared_patterns() {
    let tmp = TempDir::new().unwrap();
    let workdir = tmp.path();
    fs::create_dir_all(workdir.join("dist")).unwrap();
    fs::write(workdir.join("dist/app.js"), "built").unwrap();

    // A large unrelated tree that a full-workdir walk would traverse.
    fs::create_dir_all(workdir.join("node_modules/pkg/deep")).unwrap();
    fs::write(workdir.join("node_modules/pkg/deep/index.js"), "dep").unwrap();

    let collected = collect_outputs(workdir, &["dist/**/*".to_string()]).unwrap();
    assert_eq!(collected, vec![PathBuf::from("dist/app.js")]);
}

#[tokio::test]
async fn a_declared_output_that_was_never_produced_is_not_an_error() {
    let tmp = TempDir::new().unwrap();
    let collected = collect_outputs(tmp.path(), &["dist/**/*".to_string()]).unwrap();
    assert!(collected.is_empty());
}

#[tokio::test]
async fn overlapping_patterns_report_each_file_once() {
    let tmp = TempDir::new().unwrap();
    let workdir = tmp.path();
    fs::create_dir_all(workdir.join("dist")).unwrap();
    fs::write(workdir.join("dist/app.js"), "built").unwrap();

    let collected = collect_outputs(
        workdir,
        &["dist/**/*".to_string(), "dist/app.js".to_string()],
    )
    .unwrap();
    assert_eq!(collected, vec![PathBuf::from("dist/app.js")]);
}

#[tokio::test]
async fn a_bare_directory_output_is_collected_as_one_tree() {
    let tmp = TempDir::new().unwrap();
    let workdir = tmp.path();
    fs::create_dir_all(workdir.join("dist/sub")).unwrap();
    fs::write(workdir.join("dist/a.js"), "a").unwrap();
    fs::write(workdir.join("dist/sub/b.js"), "b").unwrap();

    let collected = collect_outputs(workdir, &["dist".to_string()]).unwrap();
    assert_eq!(collected, vec![PathBuf::from("dist")]);
}

// =============================================================================
// Run-wide cache override (CUENV_CACHE)
// =============================================================================

#[test]
fn an_override_narrows_a_read_write_task_to_read() {
    let tmp = TempDir::new().unwrap();
    let mut cache = make_cache(tmp.path());
    cache.mode_override = Some(TaskCacheMode::Read);
    let task = make_task("echo", &["hi"], &["input.txt"], &[]);

    let policy = effective_policy(&cache, &task);
    assert!(policy.mode.allows_read());
    assert!(!policy.mode.allows_write());
}

#[test]
fn an_override_can_force_write_only_to_refresh_a_poisoned_entry() {
    let tmp = TempDir::new().unwrap();
    let mut cache = make_cache(tmp.path());
    cache.mode_override = Some(TaskCacheMode::Write);
    let task = make_task("echo", &["hi"], &["input.txt"], &[]);

    let policy = effective_policy(&cache, &task);
    assert!(!policy.mode.allows_read());
    assert!(policy.mode.allows_write());
}

#[test]
fn an_override_never_caches_a_task_that_opted_out() {
    // CUENV_CACHE is a brake, not an accelerator: it must not switch caching
    // on for a task whose own policy is `never`.
    let tmp = TempDir::new().unwrap();
    let mut cache = make_cache(tmp.path());
    cache.mode_override = Some(TaskCacheMode::ReadWrite);
    let mut task = make_task("echo", &["hi"], &["input.txt"], &[]);
    task.cache = Some(TaskCachePolicy {
        mode: TaskCacheMode::Never,
        max_age: None,
    });

    let policy = effective_policy(&cache, &task);
    assert!(!policy.mode.allows_read());
    assert!(!policy.mode.allows_write());
}

#[test]
fn an_override_only_removes_permissions_a_task_declared() {
    // Every (task mode, override) pair: the effective mode grants a
    // permission only when both sides grant it.
    let modes = [
        TaskCacheMode::Never,
        TaskCacheMode::Read,
        TaskCacheMode::Write,
        TaskCacheMode::ReadWrite,
    ];
    let tmp = TempDir::new().unwrap();
    for declared in modes {
        for override_mode in [TaskCacheMode::Read, TaskCacheMode::Write] {
            let mut cache = make_cache(tmp.path());
            cache.mode_override = Some(override_mode);
            let mut task = make_task("echo", &["hi"], &["input.txt"], &[]);
            task.cache = Some(TaskCachePolicy {
                mode: declared,
                max_age: None,
            });

            let effective = effective_policy(&cache, &task).mode;

            assert_eq!(
                effective.allows_read(),
                declared.allows_read() && override_mode.allows_read(),
                "{declared:?} under {override_mode:?}"
            );
            assert_eq!(
                effective.allows_write(),
                declared.allows_write() && override_mode.allows_write(),
                "{declared:?} under {override_mode:?}"
            );
        }
    }
}

#[test]
fn without_an_override_the_task_policy_is_used_verbatim() {
    let tmp = TempDir::new().unwrap();
    let cache = make_cache(tmp.path());
    let task = make_task("echo", &["hi"], &["input.txt"], &[]);

    let policy = effective_policy(&cache, &task);
    assert!(policy.mode.allows_read());
    assert!(policy.mode.allows_write());
}

// ---------------------------------------------------------------------------
// Cross-project inputs
// ---------------------------------------------------------------------------

/// A two-project module: `producer/` and `consumer/`, with the hasher rooted
/// at the module so either project's files are reachable.
struct CrossProjectModule {
    _tmp: TempDir,
    module_root: PathBuf,
    consumer_root: PathBuf,
    cache: TaskCacheConfig,
}

fn cross_project_module() -> CrossProjectModule {
    let tmp = TempDir::new().unwrap();
    // Input resolution canonicalizes what it hashes, so the fixture must too:
    // on macOS `$TMPDIR` lives under a `/var -> /private/var` symlink.
    let module_root = tmp.path().canonicalize().unwrap();
    let producer_root = module_root.join("producer");
    let consumer_root = module_root.join("consumer");
    fs::create_dir_all(producer_root.join("dist/nested")).unwrap();
    fs::create_dir_all(&consumer_root).unwrap();
    fs::write(producer_root.join("dist/app.js"), "built").unwrap();
    fs::write(producer_root.join("dist/nested/lib.js"), "nested").unwrap();
    fs::write(consumer_root.join("main.ts"), "local").unwrap();

    let mut cache = make_cache(&module_root);
    cache
        .project_roots
        .insert("producer".to_string(), producer_root);
    CrossProjectModule {
        _tmp: tmp,
        module_root,
        consumer_root,
        cache,
    }
}

fn consuming_task(project: &str, mappings: &[(&str, &str)]) -> Task {
    let mut task = make_task("bundle", &[], &[], &[]);
    task.inputs = vec![Input::Project(cuenv_manifest::tasks::ProjectReference {
        project: project.to_string(),
        task: "build".to_string(),
        map: mappings
            .iter()
            .map(|(from, to)| cuenv_manifest::tasks::Mapping {
                from: (*from).to_string(),
                to: (*to).to_string(),
            })
            .collect(),
    })];
    task
}

async fn cross_project_digest(module: &CrossProjectModule, task: &Task) -> Option<Digest> {
    let env = Environment::new();
    build_action_for_test(BuildActionInput {
        task,
        task_name: "consumer.bundle",
        environment: &env,
        cache: &module.cache,
        workdir: &module.consumer_root,
        project_root: &module.consumer_root,
    })
    .await
    .map(|(_, digest)| digest)
}

#[tokio::test]
async fn a_relative_project_path_resolves_within_the_hasher_workspace() {
    let mut module = cross_project_module();
    module.cache.project_roots.clear();
    let task = consuming_task("../producer", &[("dist/app.js", "vendor/app.js")]);
    let env = Environment::new();

    let outcome = build_action(BuildActionInput {
        task: &task,
        task_name: "consumer.bundle",
        environment: &env,
        cache: &module.cache,
        workdir: &module.consumer_root,
        project_root: &module.consumer_root,
    })
    .await
    .unwrap();

    let CacheOutcome::Eligible(eligible) = outcome else {
        panic!("safe sibling project path should be cache eligible");
    };
    assert_eq!(
        eligible.inputs[0].absolute_path,
        module.module_root.join("producer/dist/app.js")
    );
    assert_eq!(
        eligible.inputs[0].relative_path,
        // Destinations are consumer-relative; the action frame is the
        // workspace, where the consumer lives at `consumer/`.
        PathBuf::from("consumer/vendor/app.js")
    );
}

#[tokio::test]
async fn a_relative_project_path_cannot_escape_the_hasher_workspace() {
    let mut module = cross_project_module();
    module.cache.project_roots.clear();
    let outside = TempDir::new().unwrap();
    fs::create_dir_all(outside.path().join("dist")).unwrap();
    fs::write(outside.path().join("dist/app.js"), "outside").unwrap();
    let outside_path = outside.path().canonicalize().unwrap();
    assert_eq!(module.module_root.parent(), outside_path.parent());
    let relative = PathBuf::from("../..").join(outside_path.file_name().unwrap());
    let task = consuming_task(
        &relative.to_string_lossy(),
        &[("dist/app.js", "vendor/app.js")],
    );
    let env = Environment::new();

    let outcome = build_action(BuildActionInput {
        task: &task,
        task_name: "consumer.bundle",
        environment: &env,
        cache: &module.cache,
        workdir: &module.consumer_root,
        project_root: &module.consumer_root,
    })
    .await
    .unwrap();

    assert!(matches!(
        outcome,
        CacheOutcome::Skipped {
            reason: CacheSkipReason::UnknownProject { .. },
            ..
        }
    ));
}

#[tokio::test]
async fn a_cross_project_input_is_hashed_from_the_other_project() {
    // The whole point: a consumer's key is a function of the producer's
    // bytes, so the producer emitting identical output leaves it unchanged.
    let module = cross_project_module();
    let task = consuming_task("producer", &[("dist/app.js", "vendor/app.js")]);

    let before = cross_project_digest(&module, &task).await.unwrap();
    let unchanged = cross_project_digest(&module, &task).await.unwrap();
    assert_eq!(before, unchanged, "identical bytes must key identically");

    fs::write(
        module.module_root.join("producer/dist/app.js"),
        "rebuilt differently",
    )
    .unwrap();
    let after = cross_project_digest(&module, &task).await.unwrap();
    assert_ne!(
        before, after,
        "a change in the other project must change the key"
    );
}

#[tokio::test]
async fn an_expanded_task_output_keeps_its_destination_mapping() {
    let module = cross_project_module();
    let mut task = make_task("bundle", &[], &[], &[]);
    task.inputs = vec![Input::Mapped(cuenv_manifest::tasks::MappedInput {
        source: "producer/dist/app.js".to_string(),
        destination: "vendor/app.js".to_string(),
        producer_task: None,
    })];
    let env = Environment::new();

    let outcome = build_action(BuildActionInput {
        task: &task,
        task_name: "consumer.bundle",
        environment: &env,
        cache: &module.cache,
        workdir: &module.consumer_root,
        project_root: &module.consumer_root,
    })
    .await
    .unwrap();
    let CacheOutcome::Eligible(eligible) = outcome else {
        panic!("mapped task output should be cache eligible");
    };

    assert_eq!(eligible.inputs.len(), 1);
    assert_eq!(
        eligible.inputs[0].relative_path,
        // Destinations are consumer-relative; the action frame is the
        // workspace, where the consumer lives at `consumer/`.
        PathBuf::from("consumer/vendor/app.js")
    );
    assert_eq!(
        eligible.inputs[0].absolute_path,
        module.module_root.join("producer/dist/app.js")
    );
}

#[tokio::test]
async fn a_directory_mapping_keeps_its_internal_structure() {
    let module = cross_project_module();
    let task = consuming_task("producer", &[("dist", "vendor")]);

    let before = cross_project_digest(&module, &task).await.unwrap();
    fs::write(
        module.module_root.join("producer/dist/nested/lib.js"),
        "changed",
    )
    .unwrap();
    let after = cross_project_digest(&module, &task).await.unwrap();
    assert_ne!(
        before, after,
        "a file nested under the mapped directory is part of the key"
    );
}

#[tokio::test]
async fn the_destination_path_is_part_of_the_key() {
    // Two tasks consuming the same bytes at different workspace paths see
    // different input roots, so they must not share an entry.
    let module = cross_project_module();
    let here = consuming_task("producer", &[("dist/app.js", "vendor/app.js")]);
    let there = consuming_task("producer", &[("dist/app.js", "third_party/app.js")]);

    assert_ne!(
        cross_project_digest(&module, &here).await.unwrap(),
        cross_project_digest(&module, &there).await.unwrap()
    );
}

#[tokio::test]
async fn a_local_input_and_a_cross_project_input_combine() {
    let module = cross_project_module();
    let mut task = consuming_task("producer", &[("dist/app.js", "vendor/app.js")]);
    task.inputs.push(Input::Path("main.ts".to_string()));

    let before = cross_project_digest(&module, &task).await.unwrap();
    fs::write(module.consumer_root.join("main.ts"), "edited").unwrap();
    let after = cross_project_digest(&module, &task).await.unwrap();
    assert_ne!(before, after, "the local input still contributes");
}

#[tokio::test]
async fn an_unknown_project_reference_is_not_cached() {
    let module = cross_project_module();
    let task = consuming_task("does-not-exist", &[("dist/app.js", "vendor/app.js")]);
    let env = Environment::new();

    let reason = skip_reason_for_test(BuildActionInput {
        task: &task,
        task_name: "consumer.bundle",
        environment: &env,
        cache: &module.cache,
        workdir: &module.consumer_root,
        project_root: &module.consumer_root,
    })
    .await;

    assert_eq!(
        reason,
        Some(CacheSkipReason::UnknownProject {
            project: "does-not-exist".to_string()
        })
    );
}

#[tokio::test]
async fn two_inputs_claiming_one_workspace_path_are_not_cached() {
    // Which file wins would be an ordering accident, and the key would not
    // describe what the task actually reads.
    let module = cross_project_module();
    let task = consuming_task(
        "producer",
        &[
            ("dist/app.js", "vendor.js"),
            ("dist/nested/lib.js", "vendor.js"),
        ],
    );
    let env = Environment::new();

    let reason = skip_reason_for_test(BuildActionInput {
        task: &task,
        task_name: "consumer.bundle",
        environment: &env,
        cache: &module.cache,
        workdir: &module.consumer_root,
        project_root: &module.consumer_root,
    })
    .await;

    assert_eq!(
        reason,
        Some(CacheSkipReason::InputCollision {
            path: "consumer/vendor.js".to_string()
        })
    );
}

#[tokio::test]
async fn identical_inputs_claiming_one_workspace_path_are_not_cached() {
    let module = cross_project_module();
    fs::write(
        module.module_root.join("producer/dist/nested/lib.js"),
        "built",
    )
    .unwrap();
    let task = consuming_task(
        "producer",
        &[
            ("dist/app.js", "vendor.js"),
            ("dist/nested/lib.js", "vendor.js"),
        ],
    );
    let env = Environment::new();

    let reason = skip_reason_for_test(BuildActionInput {
        task: &task,
        task_name: "consumer.bundle",
        environment: &env,
        cache: &module.cache,
        workdir: &module.consumer_root,
        project_root: &module.consumer_root,
    })
    .await;

    assert_eq!(
        reason,
        Some(CacheSkipReason::InputCollision {
            path: "consumer/vendor.js".to_string()
        })
    );
}

#[tokio::test]
async fn file_and_directory_prefix_inputs_are_not_cached() {
    let module = cross_project_module();
    let task = consuming_task(
        "producer",
        &[
            ("dist/app.js", "vendor"),
            ("dist/nested/lib.js", "vendor/lib.js"),
        ],
    );
    let env = Environment::new();

    let reason = skip_reason_for_test(BuildActionInput {
        task: &task,
        task_name: "consumer.bundle",
        environment: &env,
        cache: &module.cache,
        workdir: &module.consumer_root,
        project_root: &module.consumer_root,
    })
    .await;

    assert!(matches!(
        reason,
        Some(CacheSkipReason::InputCollision { .. })
    ));
}

#[test]
fn literal_prefix_bounds_a_pattern() {
    assert_eq!(literal_prefix("dist/**/*.js"), PathBuf::from("dist"));
    assert_eq!(literal_prefix("dist/app.js"), PathBuf::from("dist/app.js"));
    assert_eq!(
        literal_prefix("./dist/app.js"),
        PathBuf::from("dist/app.js")
    );
    assert_eq!(literal_prefix("**/*.js"), PathBuf::new());
}
