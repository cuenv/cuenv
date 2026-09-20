use super::*;
use crate::{Input, Task, TaskCacheMode, TaskCachePolicy};
use cuenv_cas::{LocalActionCache, LocalCas};
use cuenv_core::environment::Environment;
use cuenv_vcs::{HashedInput, VcsHasher, WalkHasher};
use std::fs;
use tempfile::TempDir;

struct PanicHasher;

#[async_trait::async_trait]
impl VcsHasher for PanicHasher {
    async fn resolve_and_hash(&self, _patterns: &[String]) -> cuenv_vcs::Result<Vec<HashedInput>> {
        panic!("runtime-env tasks should skip cache before hashing inputs");
    }

    fn name(&self) -> &'static str {
        "panic"
    }
}

fn make_cache(root: &Path) -> TaskCacheConfig {
    TaskCacheConfig {
        cas: Arc::new(LocalCas::open(root).unwrap()),
        action_cache: Arc::new(LocalActionCache::open(root).unwrap()),
        vcs_hasher: Arc::new(WalkHasher::new(root)),
        vcs_hasher_root: root.to_path_buf(),
        action_semantics_version: 1,
        runtime_identity_properties: BTreeMap::new(),
        cache_disabled_reason: None,
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
        CacheOutcome::Eligible(action, digest) => Some((*action, digest)),
        CacheOutcome::Skipped(_) => None,
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
        module_root: tmp.path(),
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
        module_root: tmp.path(),
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
        module_root: tmp.path(),
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
        module_root: tmp.path(),
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
        module_root: tmp.path(),
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
        module_root: tmp.path(),
    })
    .await;

    assert!(result.is_none());
    assert!(!side_effect.exists());
}

#[tokio::test]
async fn build_action_skips_runtime_env_before_hashing_inputs() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("input.txt"), "payload").unwrap();
    let mut cache = make_cache(tmp.path());
    cache.vcs_hasher = Arc::new(PanicHasher);

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
        module_root: tmp.path(),
    })
    .await
    .unwrap();

    assert!(matches!(
        outcome,
        CacheOutcome::Skipped(CacheSkipReason::RuntimeEnv)
    ));
}

#[tokio::test]
async fn build_action_runtime_env_skip_precedes_hasher_root_mismatch() {
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
        module_root: tmp.path(),
    })
    .await
    .unwrap();

    assert!(matches!(
        outcome,
        CacheOutcome::Skipped(CacheSkipReason::RuntimeEnv)
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
        module_root: workspace_root,
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
        module_root: workspace_root,
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
        module_root: workspace_root,
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
        module_root: tmp.path(),
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
        module_root: tmp.path(),
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
        module_root: tmp.path(),
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
        module_root: tmp.path(),
    })
    .await
    .unwrap();

    assert_ne!(first, second);
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
        module_root: tmp.path(),
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

    let recorded = lookup(&cache, &action_digest, &task).await.unwrap().unwrap();
    assert_eq!(recorded.exit_code, 0);
    assert_eq!(recorded.output_files.len(), 1);
    assert_eq!(recorded.output_files[0].path, "out.txt");

    let fresh = tmp.path().join("fresh");
    fs::create_dir_all(&fresh).unwrap();
    let (stdout, stderr, exit_code) = materialize_hit(&cache, &fresh, &recorded).await.unwrap();
    assert_eq!(stdout, "stdout-text");
    assert_eq!(stderr, "stderr-text");
    assert_eq!(exit_code, 0);
    assert_eq!(fs::read(fresh.join("out.txt")).unwrap(), b"produced");
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
        module_root: tmp.path(),
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

    let recorded = lookup(&cache, &action_digest, &task).await.unwrap().unwrap();
    let fresh = tmp.path().join("fresh");
    fs::create_dir_all(&fresh).unwrap();
    materialize_hit(&cache, &fresh, &recorded).await.unwrap();

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
        module_root: tmp.path(),
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
        module_root: tmp.path(),
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
        module_root: tmp.path(),
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
        module_root: tmp.path(),
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
        CacheOutcome::Eligible(..) => None,
        CacheOutcome::Skipped(reason) => Some(reason),
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
        module_root: tmp.path(),
    })
    .await;

    assert_eq!(reason, Some(CacheSkipReason::NonHermetic));
}

#[tokio::test]
async fn build_action_skips_workdir_outside_project_and_module_roots() {
    let tmp = TempDir::new().unwrap();
    let project = tmp.path().join("project");
    let elsewhere = tmp.path().join("elsewhere");
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
        // Under neither the project nor the module root: normalizing would
        // bake an absolute host path into the key.
        workdir: &elsewhere,
        project_root: &project,
        module_root: &project,
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
        module_root: tmp.path(),
    })
    .await
    .unwrap();

    // Re-decode the stored Command blob: it is what the digest was taken over,
    // and it is REAPI protobuf, exactly as a remote server would store it.
    let bytes = cache.cas.get(&action.command_digest).await.unwrap();
    let command = decode_command(&bytes);

    assert_eq!(
        command.environment_variables.get("DECLARED").map(String::as_str),
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
        module_root: tmp.path(),
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
async fn lookup_ignores_entry_whose_output_blob_was_evicted() {
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
    assert!(lookup(&cache, &action_digest, &task).await.unwrap().is_some());

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
        lookup(&cache, &action_digest, &task).await.unwrap().is_none(),
        "a dangling entry must degrade to a miss, not a partial restore"
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

    let error = materialize_hit(&cache, &workdir, &result).await.unwrap_err();
    assert!(
        error.to_string().contains("stay inside the working directory"),
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

    assert!(materialize_hit(&cache, &workdir, &result).await.is_err());

    // Staging means the first output is never installed, so the workspace is
    // not left as a mix of cached and pre-existing files.
    assert_eq!(fs::read_to_string(workdir.join("first.txt")).unwrap(), "original first");
    assert_eq!(fs::read_to_string(workdir.join("second.txt")).unwrap(), "original second");
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

    materialize_hit(&cache, &workdir, &result).await.unwrap();

    assert_eq!(
        fs::read_to_string(workdir.join("nested/out.txt")).unwrap(),
        "restored"
    );
    let leftovers: Vec<_> = fs::read_dir(&workdir)
        .unwrap()
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(STAGING_PREFIX))
        .collect();
    assert!(leftovers.is_empty(), "staging directory was left behind");
}
