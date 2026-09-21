//! End-to-end test that the executor's cache wrapper actually
//! short-circuits a second invocation of the same task.
//!
//! A cache-eligible task runs in a directory sandbox, so an undeclared
//! write never reaches the workspace and cannot prove that a process
//! ran. The proof is a nonce the process prints: a cache hit replays the
//! first run's stdout, and a miss prints a new one. `marker.txt` stays in
//! the script so the test also asserts the other half of the contract —
//! an undeclared write is discarded rather than leaking into the checkout.

use cuenv_cas::{LocalActionCache, LocalCas};
use cuenv_core::OutputCapture;
use cuenv_task_exec::cache::TaskCacheConfig;
use cuenv_task_exec::executor::{ExecutorConfig, TaskExecutor};
use cuenv_task_exec::{Input, Task, TaskCacheMode, TaskCachePolicy};
use cuenv_vcs::WalkHasher;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use tempfile::TempDir;

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
        ..Default::default()
    };
    TaskExecutor::new(config)
}

/// Print a fresh nonce, copy the declared input to the declared output,
/// and touch a file the task did not declare.
fn cached_script() -> String {
    "cat /proc/sys/kernel/random/uuid; cat input.txt > out.txt; touch marker.txt".to_string()
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
        args: vec![
            "-c".to_string(),
            "cat /proc/sys/kernel/random/uuid; touch marker.txt".to_string(),
        ],
        // no `inputs` — not cache-eligible, so the inherited sandbox steps aside
        ..Task::default()
    };

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
