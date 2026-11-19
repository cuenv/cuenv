//! Integration tests for strict OS sandboxing, `auto` fallback, descendant
//! reaping on successful exit, and `TaskResult` cache provenance.

use cuenv_cas::{ACTION_SEMANTICS_VERSION, LocalActionCache, LocalCas};
use cuenv_core::OutputCapture;
use cuenv_core::environment::Environment;
use cuenv_core::tasks::{
    Hermetic, HermeticOptions, Input, Sandbox, Task, TaskCacheMode, TaskCachePolicy,
};
use cuenv_task_exec::cache::TaskCacheConfig;
use cuenv_task_exec::sandbox::{EffectiveSandbox, resolve_sandbox};
use cuenv_task_exec::{CacheSource, ExecutorConfig, TaskExecutor};
use cuenv_vcs::WalkHasher;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;
use tempfile::TempDir;

fn build_executor(workspace: &Path, cache_root: Option<&Path>) -> cuenv_cas::Result<TaskExecutor> {
    let mut environment = Environment::new();
    if let Ok(path) = std::env::var("PATH") {
        environment.set("PATH".to_string(), path);
    }

    let cache = match cache_root {
        Some(root) => Some(TaskCacheConfig {
            cas: Arc::new(LocalCas::open(root)?),
            action_cache: Arc::new(LocalActionCache::open(root)?),
            vcs_hasher: Arc::new(WalkHasher::new(workspace).excluding(root)),
            vcs_hasher_root: workspace.to_path_buf(),
            action_semantics_version: ACTION_SEMANTICS_VERSION,
            runtime_identity_properties: BTreeMap::new(),
            cache_disabled_reason: None,
            secret_salt: None,
            mode_override: None,
            cache_root: root.to_path_buf(),
            publisher: None,
        }),
        None => None,
    };

    Ok(TaskExecutor::new(ExecutorConfig {
        capture_output: OutputCapture::Capture,
        project_root: workspace.to_path_buf(),
        cue_module_root: Some(workspace.to_path_buf()),
        environment,
        cache,
        sandbox_hasher_root: Some(workspace.to_path_buf()),
        ..ExecutorConfig::default()
    }))
}

fn shell_task(script: &str, sandbox: Sandbox, cache_mode: TaskCacheMode) -> Task {
    Task {
        command: "sh".to_string(),
        args: vec!["-c".to_string(), script.to_string()],
        inputs: vec![Input::Path("in.txt".to_string())],
        outputs: vec!["out.txt".to_string()],
        cache: Some(TaskCachePolicy {
            mode: cache_mode,
            max_age: None,
        }),
        hermetic: Hermetic::Options(HermeticOptions {
            passthrough: Vec::new(),
            sandbox: Some(sandbox),
            network: false,
        }),
        ..Task::default()
    }
}

#[tokio::test]
async fn strict_sandbox_fails_closed_even_when_caching_is_disabled() {
    let workspace = TempDir::new().unwrap();
    fs::write(workspace.path().join("in.txt"), "hello").unwrap();

    let strict_probe = resolve_sandbox(Sandbox::Strict, false);
    if strict_probe.is_ok() {
        // Host supports strict confinement: task should succeed and isolate.
        let executor = build_executor(workspace.path(), None).unwrap();
        let task = shell_task(
            "cat in.txt > out.txt",
            Sandbox::Strict,
            TaskCacheMode::Never,
        );
        let result = executor.execute_task("strict-ok", &task).await.unwrap();
        assert!(result.success, "{}", result.stderr);
        assert_eq!(
            fs::read_to_string(workspace.path().join("out.txt")).unwrap(),
            "hello"
        );
        return;
    }

    // Host does not support strict confinement: must fail closed even with cache: None.
    let executor = build_executor(workspace.path(), None).unwrap();
    let task = shell_task(
        "cat in.txt > out.txt",
        Sandbox::Strict,
        TaskCacheMode::Never,
    );
    let err = executor
        .execute_task("strict-fail", &task)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("strict"),
        "expected strict fail-closed error, got: {err}"
    );
    assert!(!workspace.path().join("out.txt").exists());
}

#[tokio::test]
async fn auto_sandbox_runs_and_caches_with_from_cache_provenance() {
    let workspace = TempDir::new().unwrap();
    let cache_root = TempDir::new().unwrap();
    fs::write(workspace.path().join("in.txt"), "payload").unwrap();

    let resolution = resolve_sandbox(Sandbox::Auto, false).unwrap();
    let expected_strict = matches!(resolution.effective, EffectiveSandbox::Strict(_));

    let executor = build_executor(workspace.path(), Some(cache_root.path())).unwrap();
    let task = shell_task(
        "cat in.txt > out.txt",
        Sandbox::Auto,
        TaskCacheMode::ReadWrite,
    );

    let first = executor.execute_task("auto-cached", &task).await.unwrap();
    assert!(first.success, "{}", first.stderr);
    assert!(!first.from_cache);
    assert_eq!(first.cache_source, None);

    // Delete projected output and re-run: must restore from cache.
    fs::remove_file(workspace.path().join("out.txt")).unwrap();
    let second = executor.execute_task("auto-cached", &task).await.unwrap();
    assert!(second.success);
    assert!(second.from_cache);
    assert_eq!(second.cache_source, Some(CacheSource::Local));
    assert_eq!(
        fs::read_to_string(workspace.path().join("out.txt")).unwrap(),
        "payload"
    );

    if !expected_strict {
        assert!(resolution.fallback_reason.is_some());
    }
}

#[cfg(unix)]
#[tokio::test]
async fn lingering_descendant_is_reaped_on_successful_exit() {
    let workspace = TempDir::new().unwrap();
    let cache_root = TempDir::new().unwrap();
    fs::write(workspace.path().join("in.txt"), "seed").unwrap();

    let executor = build_executor(workspace.path(), Some(cache_root.path())).unwrap();
    let pid_file = workspace.path().join("bg.pid");
    let leaked_file = workspace.path().join("leaked.txt");
    let script = format!(
        "(sleep 0.5 && echo leaked > '{}') >/dev/null 2>&1 & echo $! > '{}' && echo done > out.txt",
        leaked_file.display(),
        pid_file.display()
    );
    let task = shell_task(&script, Sandbox::Dir, TaskCacheMode::Never);

    let result = executor.execute_task("reap-bg", &task).await.unwrap();
    assert!(result.success, "{}", result.stderr);

    let pid: i32 = fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // Signal 0 checks whether the background process is still alive. On Linux
    // containers without an orphan reaper, a killed grandchild may linger
    // briefly in state 'Z' (zombie), which is already terminated.
    #[expect(unsafe_code, reason = "Test verifies background descendant was reaped")]
    let rc = unsafe { libc::kill(pid, 0) };
    let is_zombie = {
        #[cfg(target_os = "linux")]
        {
            fs::read_to_string(format!("/proc/{pid}/stat"))
                .ok()
                .and_then(|stat| {
                    stat.rsplit_once(')')
                        .map(|(_, rest)| rest.trim().starts_with('Z'))
                })
                .unwrap_or(false)
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    };
    assert!(
        rc == -1 || is_zombie,
        "background descendant pid {pid} should have been terminated after task exited 0 (kill={rc}, zombie={is_zombie})"
    );
    // Confirm the delayed side effect never fires after execute_task returns.
    tokio::time::sleep(std::time::Duration::from_millis(650)).await;
    assert!(
        !leaked_file.exists(),
        "terminated descendant must not write after task exit"
    );
}

struct CountingPublisher {
    calls: std::sync::atomic::AtomicUsize,
}

impl cuenv_cas::ActionPublisher for CountingPublisher {
    fn publish<'life0, 'life1, 'async_trait>(
        &'life0 self,
        _request: cuenv_cas::PublishActionRequest<'life1>,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = cuenv_cas::Result<()>> + Send + 'async_trait>,
    >
    where
        'life0: 'async_trait,
        'life1: 'async_trait,
        Self: 'async_trait,
    {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test]
async fn zero_remote_writes_on_hits_failures_non_strict_network_and_secrets() {
    let workspace = TempDir::new().unwrap();
    let cache_root = TempDir::new().unwrap();
    fs::write(workspace.path().join("in.txt"), "seed").unwrap();

    let publisher = Arc::new(CountingPublisher {
        calls: std::sync::atomic::AtomicUsize::new(0),
    });

    let mut environment = Environment::new();
    if let Ok(path) = std::env::var("PATH") {
        environment.set("PATH".to_string(), path);
    }
    environment.set_secret("API_TOKEN".to_string(), "super-secret-value".to_string());

    let cache = TaskCacheConfig {
        cas: Arc::new(LocalCas::open(cache_root.path()).unwrap()),
        action_cache: Arc::new(LocalActionCache::open(cache_root.path()).unwrap()),
        vcs_hasher: Arc::new(WalkHasher::new(workspace.path()).excluding(cache_root.path())),
        vcs_hasher_root: workspace.path().to_path_buf(),
        action_semantics_version: ACTION_SEMANTICS_VERSION,
        runtime_identity_properties: BTreeMap::new(),
        cache_disabled_reason: None,
        secret_salt: Some("test-salt".to_string()),
        mode_override: None,
        cache_root: cache_root.path().to_path_buf(),
        publisher: Some(publisher.clone()),
    };

    let executor = TaskExecutor::new(ExecutorConfig {
        capture_output: OutputCapture::Capture,
        project_root: workspace.path().to_path_buf(),
        cue_module_root: Some(workspace.path().to_path_buf()),
        environment,
        cache: Some(cache),
        sandbox_hasher_root: Some(workspace.path().to_path_buf()),
        ..ExecutorConfig::default()
    });

    // 1. Sandbox::Dir miss + cache hit -> 0 remote writes.
    let dir_task = shell_task("echo ok > out.txt", Sandbox::Dir, TaskCacheMode::ReadWrite);
    let r1 = executor.execute_task("dir-miss", &dir_task).await.unwrap();
    assert!(r1.success);
    assert!(!r1.from_cache);
    let r2 = executor.execute_task("dir-hit", &dir_task).await.unwrap();
    assert!(r2.success);
    assert!(r2.from_cache);

    // 2. Failing task -> 0 remote writes.
    let fail_task = shell_task("exit 1", Sandbox::Dir, TaskCacheMode::ReadWrite);
    let r3 = executor
        .execute_task("fail-task", &fail_task)
        .await
        .unwrap();
    assert!(!r3.success);

    // 3. Network-enabled task -> 0 remote writes.
    let mut net_task = shell_task(
        "echo net > out.txt",
        Sandbox::Auto,
        TaskCacheMode::ReadWrite,
    );
    net_task.hermetic = Hermetic::Options(HermeticOptions {
        sandbox: Some(Sandbox::Auto),
        network: true,
        ..Default::default()
    });
    let r4 = executor.execute_task("net-task", &net_task).await.unwrap();
    assert!(r4.success);

    assert_eq!(
        publisher.calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "remote publisher must not be called for dir, cache hits, failures, network-enabled, or secret-bearing tasks"
    );
}
