//! End-to-end test that the executor's cache wrapper actually
//! short-circuits a second invocation of the same task.
//!
//! A cache-eligible task runs in a directory sandbox by default, so an
//! undeclared write never reaches the workspace and cannot prove that a
//! process ran. The proof is a nonce the process prints: a cache hit
//! replays the first run's stdout, and a miss prints a new one.
//! `marker.txt` stays in the script so the tests also assert the other
//! half of the contract — under the sandbox an undeclared write is
//! discarded rather than leaking into the checkout, and with
//! `sandbox: "none"` it lands in the checkout on a run and is left alone
//! on a hit.

use cuenv_cas::{LocalActionCache, LocalCas};
use cuenv_core::OutputCapture;
use cuenv_core::environment::Environment;
use cuenv_core::tasks::{Hermetic, HermeticOptions, Sandbox};
use cuenv_task_exec::cache::TaskCacheConfig;
use cuenv_task_exec::executor::{ExecutorConfig, TaskExecutor};
use cuenv_task_exec::{Input, Task, TaskCacheMode, TaskCachePolicy};
use cuenv_vcs::WalkHasher;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use tempfile::TempDir;

/// A hermetic task receives only the declared environment, so without this
/// `sh` has no `PATH` and cannot find `cat`. The Nix Linux sandbox happens to
/// paper over that with a standalone busybox `/bin/sh`; a macOS checkout does
/// not. Declaring the host `PATH` makes the tests mean the same thing on both.
fn host_path_environment() -> Environment {
    let mut environment = Environment::new();
    environment.set(
        "PATH".to_string(),
        std::env::var("PATH").unwrap_or_default(),
    );
    environment
}

fn build_executor(workspace: &Path, cache_root: &Path) -> TaskExecutor {
    let cache = TaskCacheConfig {
        cas: Arc::new(LocalCas::open(cache_root).unwrap()),
        action_cache: Arc::new(LocalActionCache::open(cache_root).unwrap()),
        vcs_hasher: Arc::new(WalkHasher::new(workspace)),
        vcs_hasher_root: workspace.to_path_buf(),
        action_semantics_version: 1,
        runtime_identity_properties: BTreeMap::new(),
        cache_disabled_reason: None,
        secret_salt: Some("test-salt".to_string()),
        mode_override: None,
        cache_root: cache_root.to_path_buf(),
        project_roots: BTreeMap::new(),
    };

    let config = ExecutorConfig {
        capture_output: OutputCapture::Capture,
        project_root: workspace.to_path_buf(),
        cache: Some(cache),
        environment: host_path_environment(),
        ..Default::default()
    };
    TaskExecutor::new(config)
}

/// A shell fragment that prints 16 random bytes as hex. `/dev/urandom` and
/// `od` are POSIX, so this runs on macOS as well as in the Linux CI sandbox;
/// `/proc/sys/kernel/random/uuid` would not.
const PRINT_NONCE: &str = "od -An -N16 -tx1 /dev/urandom | tr -d ' \\n'; echo";

/// Print a fresh nonce, copy the declared input to the declared output,
/// and touch a file the task did not declare.
fn cached_script() -> String {
    format!("{PRINT_NONCE}; cat input.txt > out.txt; touch marker.txt")
}

fn cached_task(script: String) -> Task {
    Task {
        command: "sh".to_string(),
        args: vec!["-c".to_string(), script],
        inputs: vec![Input::Path("input.txt".to_string())],
        outputs: vec!["out.txt".to_string()],
        cache: Some(TaskCachePolicy {
            mode: TaskCacheMode::ReadWrite,
            max_age: None,
        }),
        ..Task::default()
    }
}

fn nonce(stdout: &str) -> &str {
    stdout.trim()
}

fn unsandboxed(task: Task) -> Task {
    Task {
        hermetic: Hermetic::Options(HermeticOptions {
            passthrough: Vec::new(),
            sandbox: Some(Sandbox::None),
        }),
        ..task
    }
}

#[tokio::test]
async fn second_run_with_unchanged_inputs_is_a_cache_hit() {
    let workspace = TempDir::new().unwrap();
    let cache_root = TempDir::new().unwrap();
    fs::write(workspace.path().join("input.txt"), "v1").unwrap();

    let executor = build_executor(workspace.path(), cache_root.path());
    let task = cached_task(cached_script());

    let result1 = executor.execute_task("touch", &task).await.unwrap();
    assert!(result1.success, "stderr: {}", result1.stderr);
    let first_nonce = nonce(&result1.stdout).to_string();
    assert!(!first_nonce.is_empty(), "the process must print a nonce");
    assert_eq!(fs::read(workspace.path().join("out.txt")).unwrap(), b"v1");
    assert!(
        !workspace.path().join("marker.txt").exists(),
        "an undeclared write must not land in the workspace"
    );

    fs::remove_file(workspace.path().join("out.txt")).unwrap();

    let result2 = executor.execute_task("touch", &task).await.unwrap();
    assert!(result2.success);
    assert_eq!(
        nonce(&result2.stdout),
        first_nonce,
        "a new nonce means the task re-ran instead of hitting the cache"
    );
    assert!(
        workspace.path().join("out.txt").exists(),
        "out.txt was not materialized from the cache"
    );
    assert_eq!(fs::read(workspace.path().join("out.txt")).unwrap(), b"v1");
    assert!(!workspace.path().join("marker.txt").exists());
}

#[tokio::test]
async fn cache_invalidates_when_input_changes() {
    let workspace = TempDir::new().unwrap();
    let cache_root = TempDir::new().unwrap();
    fs::write(workspace.path().join("input.txt"), "v1").unwrap();

    let executor = build_executor(workspace.path(), cache_root.path());
    let task = cached_task(cached_script());

    let result1 = executor.execute_task("t", &task).await.unwrap();
    assert!(result1.success, "stderr: {}", result1.stderr);
    let first_nonce = nonce(&result1.stdout).to_string();

    fs::write(workspace.path().join("input.txt"), "v2").unwrap();

    let result2 = executor.execute_task("t", &task).await.unwrap();
    assert!(result2.success, "stderr: {}", result2.stderr);
    assert_ne!(
        nonce(&result2.stdout),
        first_nonce,
        "input change should have triggered a real re-execution"
    );
    assert_eq!(fs::read(workspace.path().join("out.txt")).unwrap(), b"v2");
    assert!(
        !workspace.path().join("marker.txt").exists(),
        "the re-run is still sandboxed, so the undeclared write stays discarded"
    );
}

#[tokio::test]
async fn task_without_inputs_is_never_cached() {
    let workspace = TempDir::new().unwrap();
    let cache_root = TempDir::new().unwrap();

    let executor = build_executor(workspace.path(), cache_root.path());
    let task = Task {
        command: "sh".to_string(),
        args: vec!["-c".to_string(), format!("{PRINT_NONCE}; touch marker.txt")],
        // no `inputs`: not cache-eligible. The default sandbox still applies,
        // over an empty input root, so the undeclared write is discarded.
        ..Task::default()
    };

    let result1 = executor.execute_task("no-inputs", &task).await.unwrap();
    assert!(result1.success, "stderr: {}", result1.stderr);
    let first_nonce = nonce(&result1.stdout).to_string();
    assert!(!first_nonce.is_empty(), "the process must print a nonce");
    assert!(
        !workspace.path().join("marker.txt").exists(),
        "an undeclared write must not land in the workspace"
    );

    let result2 = executor.execute_task("no-inputs", &task).await.unwrap();
    assert!(result2.success, "stderr: {}", result2.stderr);
    assert_ne!(
        nonce(&result2.stdout),
        first_nonce,
        "a task without declared inputs must always re-run"
    );
}

#[tokio::test]
async fn an_unsandboxed_task_without_inputs_always_reruns_in_the_workspace() {
    // The same task with `sandbox: "none"` really runs in the checkout, so
    // its side effect is visible and must be regenerated on every run.
    let workspace = TempDir::new().unwrap();
    let cache_root = TempDir::new().unwrap();

    let executor = build_executor(workspace.path(), cache_root.path());
    let task = unsandboxed(Task {
        command: "sh".to_string(),
        args: vec!["-c".to_string(), format!("{PRINT_NONCE}; touch marker.txt")],
        ..Task::default()
    });

    let result1 = executor.execute_task("no-inputs", &task).await.unwrap();
    assert!(result1.success, "stderr: {}", result1.stderr);
    let first_nonce = nonce(&result1.stdout).to_string();
    assert!(workspace.path().join("marker.txt").exists());
    fs::remove_file(workspace.path().join("marker.txt")).unwrap();

    let result2 = executor.execute_task("no-inputs", &task).await.unwrap();
    assert!(result2.success, "stderr: {}", result2.stderr);
    assert_ne!(
        nonce(&result2.stdout),
        first_nonce,
        "a task without declared inputs must always re-run"
    );
    assert!(
        workspace.path().join("marker.txt").exists(),
        "task without declared inputs must always re-run in the workspace"
    );
}

#[tokio::test]
async fn an_unsandboxed_task_still_hits_the_cache_without_spawning() {
    // `sandbox: "none"` is the escape hatch for tasks that must touch the
    // live checkout. It must not also be an escape hatch from the cache:
    // the second run is served from the action cache, so the undeclared
    // side effect the first run left in the workspace is not recreated.
    let workspace = TempDir::new().unwrap();
    let cache_root = TempDir::new().unwrap();
    fs::write(workspace.path().join("input.txt"), "v1").unwrap();

    let executor = build_executor(workspace.path(), cache_root.path());
    let task = unsandboxed(cached_task(cached_script()));

    let result1 = executor.execute_task("live", &task).await.unwrap();
    assert!(result1.success, "stderr: {}", result1.stderr);
    let first_nonce = nonce(&result1.stdout).to_string();
    assert!(!first_nonce.is_empty(), "the process must print a nonce");
    assert!(
        workspace.path().join("marker.txt").exists(),
        "sandbox: \"none\" runs in the checkout, so the undeclared write lands there"
    );
    assert_eq!(fs::read(workspace.path().join("out.txt")).unwrap(), b"v1");

    fs::remove_file(workspace.path().join("marker.txt")).unwrap();
    fs::remove_file(workspace.path().join("out.txt")).unwrap();

    let result2 = executor.execute_task("live", &task).await.unwrap();
    assert!(result2.success, "stderr: {}", result2.stderr);
    assert_eq!(
        nonce(&result2.stdout),
        first_nonce,
        "a new nonce means the task re-ran instead of hitting the cache"
    );
    assert!(
        !workspace.path().join("marker.txt").exists(),
        "marker.txt was recreated, so a process ran on what should be a cache hit"
    );
    assert_eq!(
        fs::read(workspace.path().join("out.txt")).unwrap(),
        b"v1",
        "the declared output must be materialized from the cache"
    );
}

#[tokio::test]
async fn changing_the_sandbox_tier_is_a_cache_miss() {
    // An entry recorded under `sandbox: "none"` may depend on files the
    // action key never saw, so it must not satisfy the sandboxed key for
    // the same command and inputs. The tier is part of the action's
    // platform identity, and this pins that: an entry from the checkout
    // is never handed to a task that asked for isolation, or vice versa.
    let workspace = TempDir::new().unwrap();
    let cache_root = TempDir::new().unwrap();
    fs::write(workspace.path().join("input.txt"), "v1").unwrap();

    let executor = build_executor(workspace.path(), cache_root.path());
    let sandboxed = cached_task(cached_script());

    let result1 = executor
        .execute_task("t", &unsandboxed(sandboxed.clone()))
        .await
        .unwrap();
    assert!(result1.success, "stderr: {}", result1.stderr);
    let live_nonce = nonce(&result1.stdout).to_string();
    fs::remove_file(workspace.path().join("marker.txt")).unwrap();

    let result2 = executor.execute_task("t", &sandboxed).await.unwrap();
    assert!(result2.success, "stderr: {}", result2.stderr);
    assert_ne!(
        nonce(&result2.stdout),
        live_nonce,
        "an entry recorded without isolation must not be served to a sandboxed task"
    );
    assert!(
        !workspace.path().join("marker.txt").exists(),
        "the sandboxed re-run discards its undeclared write"
    );
    assert_eq!(fs::read(workspace.path().join("out.txt")).unwrap(), b"v1");

    // And the sandboxed entry is now its own hit.
    fs::remove_file(workspace.path().join("out.txt")).unwrap();
    let result3 = executor.execute_task("t", &sandboxed).await.unwrap();
    assert!(result3.success, "stderr: {}", result3.stderr);
    assert_eq!(nonce(&result3.stdout), nonce(&result2.stdout));
    assert_eq!(fs::read(workspace.path().join("out.txt")).unwrap(), b"v1");
}
