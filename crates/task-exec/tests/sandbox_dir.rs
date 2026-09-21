//! End-to-end tests for `hermetic.sandbox: "dir"`.
//!
//! The claim under test is the one that makes a cache entry worth uploading:
//! a task running under directory isolation sees exactly its declared inputs,
//! and only its declared outputs come back. Everything else here — that an
//! undeclared read fails, that an undeclared write is lost — follows from
//! that and is checked because a sandbox nobody verifies is a sandbox nobody
//! should trust.

use cuenv_cas::{LocalActionCache, LocalCas};
use cuenv_core::OutputCapture;
use cuenv_core::tasks::{Hermetic, HermeticOptions, Sandbox};
use cuenv_task_exec::cache::TaskCacheConfig;
use cuenv_task_exec::executor::{ExecutorConfig, TaskExecutor};
use cuenv_task_exec::{
    Input, Task, TaskCacheMode, TaskCachePolicy, TaskDirectory, TaskDirectoryBase,
};
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

    TaskExecutor::new(ExecutorConfig {
        capture_output: OutputCapture::Capture,
        project_root: workspace.to_path_buf(),
        cache: Some(cache),
        ..Default::default()
    })
}

/// A task that inherits whatever the default isolation tier is, rather than
/// naming one. This is what the overwhelming majority of real tasks look like.
fn defaulted(script: &str, inputs: &[&str], outputs: &[&str]) -> Task {
    Task {
        hermetic: Hermetic::Enabled(true),
        ..sandboxed(script, inputs, outputs)
    }
}

fn sandboxed(script: &str, inputs: &[&str], outputs: &[&str]) -> Task {
    Task {
        command: "sh".to_string(),
        args: vec!["-c".to_string(), script.to_string()],
        inputs: inputs
            .iter()
            .map(|path| Input::Path((*path).to_string()))
            .collect(),
        outputs: outputs.iter().map(|output| (*output).to_string()).collect(),
        cache: Some(TaskCachePolicy {
            mode: TaskCacheMode::ReadWrite,
            max_age: None,
        }),
        hermetic: Hermetic::Options(HermeticOptions {
            passthrough: Vec::new(),
            sandbox: Some(Sandbox::Dir),
        }),
        ..Task::default()
    }
}

#[tokio::test]
async fn a_sandboxed_task_sees_its_declared_inputs() {
    let workspace = TempDir::new().unwrap();
    let cache_root = TempDir::new().unwrap();
    fs::write(workspace.path().join("declared.txt"), "visible").unwrap();

    let executor = build_executor(workspace.path(), cache_root.path());
    let task = sandboxed("cat declared.txt", &["declared.txt"], &[]);

    let result = executor.execute_task("read-declared", &task).await.unwrap();
    assert!(result.success, "stderr: {}", result.stderr);
    assert!(result.stdout.contains("visible"));
}

#[tokio::test]
async fn a_sandboxed_task_cannot_read_an_undeclared_file() {
    // This is the whole point. Without isolation the task succeeds, records
    // an entry keyed on `declared.txt` alone, and that entry is wrong on any
    // machine where `undeclared.txt` differs.
    let workspace = TempDir::new().unwrap();
    let cache_root = TempDir::new().unwrap();
    fs::write(workspace.path().join("declared.txt"), "visible").unwrap();
    fs::write(workspace.path().join("undeclared.txt"), "invisible").unwrap();

    let executor = build_executor(workspace.path(), cache_root.path());
    let task = sandboxed("cat undeclared.txt", &["declared.txt"], &[]);

    let result = executor
        .execute_task("read-undeclared", &task)
        .await
        .unwrap();
    assert!(
        !result.success,
        "an undeclared read must fail, not silently succeed"
    );
}

#[tokio::test]
async fn a_hermetic_task_does_not_inherit_undeclared_host_environment() {
    let workspace = TempDir::new().unwrap();
    let cache_root = TempDir::new().unwrap();
    let executor = build_executor(workspace.path(), cache_root.path());
    let task = sandboxed(r#"test -z "${HOME+x}""#, &[], &[]);

    let result = executor.execute_task("clean-env", &task).await.unwrap();
    assert!(result.success, "stderr: {}", result.stderr);
}

#[tokio::test]
async fn a_declared_output_is_projected_back_into_the_workspace() {
    let workspace = TempDir::new().unwrap();
    let cache_root = TempDir::new().unwrap();
    fs::write(workspace.path().join("src.txt"), "source").unwrap();

    let executor = build_executor(workspace.path(), cache_root.path());
    let task = sandboxed("mkdir -p out && cp src.txt out/built.txt", &["src.txt"], &[
        "out/built.txt",
    ]);

    let result = executor.execute_task("build", &task).await.unwrap();
    assert!(result.success, "stderr: {}", result.stderr);
    assert_eq!(
        fs::read_to_string(workspace.path().join("out/built.txt")).unwrap(),
        "source",
        "a build that produces nothing the user can see is not a build"
    );
}

#[tokio::test]
async fn an_undeclared_write_does_not_reach_the_workspace() {
    let workspace = TempDir::new().unwrap();
    let cache_root = TempDir::new().unwrap();
    fs::write(workspace.path().join("src.txt"), "source").unwrap();

    let executor = build_executor(workspace.path(), cache_root.path());
    let task = sandboxed("echo scratch > scratch.tmp", &["src.txt"], &[]);

    let result = executor.execute_task("scratch", &task).await.unwrap();
    assert!(result.success);
    assert!(
        !workspace.path().join("scratch.tmp").exists(),
        "an undeclared write is lost visibly on the first run, not mysteriously later"
    );
}

#[tokio::test]
async fn the_exec_root_does_not_outlive_the_run() {
    let workspace = TempDir::new().unwrap();
    let cache_root = TempDir::new().unwrap();
    fs::write(workspace.path().join("src.txt"), "source").unwrap();

    let executor = build_executor(workspace.path(), cache_root.path());
    let task = sandboxed("true", &["src.txt"], &[]);
    executor.execute_task("noop", &task).await.unwrap();

    let exec = cache_root.path().join("exec");
    let leftovers = if exec.exists() {
        fs::read_dir(&exec).unwrap().count()
    } else {
        0
    };
    assert_eq!(leftovers, 0, "exec roots must not accumulate");
}

#[tokio::test]
async fn a_failing_sandboxed_task_still_cleans_up() {
    let workspace = TempDir::new().unwrap();
    let cache_root = TempDir::new().unwrap();
    fs::write(workspace.path().join("src.txt"), "source").unwrap();

    let executor = build_executor(workspace.path(), cache_root.path());
    let task = sandboxed("exit 1", &["src.txt"], &[]);
    let result = executor.execute_task("fails", &task).await.unwrap();
    assert!(!result.success);

    let exec = cache_root.path().join("exec");
    let leftovers = if exec.exists() {
        fs::read_dir(&exec).unwrap().count()
    } else {
        0
    };
    assert_eq!(leftovers, 0);
}

#[tokio::test]
async fn a_failing_task_does_not_replace_the_last_good_output() {
    let workspace = TempDir::new().unwrap();
    let cache_root = TempDir::new().unwrap();
    fs::write(workspace.path().join("src.txt"), "source").unwrap();
    fs::create_dir_all(workspace.path().join("dist")).unwrap();
    fs::write(workspace.path().join("dist/app.txt"), "last-good").unwrap();

    let executor = build_executor(workspace.path(), cache_root.path());
    let task = sandboxed(
        "mkdir -p dist; echo partial > dist/app.txt; exit 1",
        &["src.txt"],
        &["dist"],
    );
    let result = executor.execute_task("failed-build", &task).await.unwrap();

    assert!(!result.success);
    assert_eq!(
        fs::read_to_string(workspace.path().join("dist/app.txt")).unwrap(),
        "last-good"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlinked_output_ancestor_is_rejected() {
    let workspace = TempDir::new().unwrap();
    let cache_root = TempDir::new().unwrap();
    let executor = build_executor(workspace.path(), cache_root.path());
    let task = sandboxed(
        "mkdir real; ln -s real link; echo built > link/app.txt",
        &[],
        &["link/app.txt"],
    );

    let error = executor
        .execute_task("symlink-output", &task)
        .await
        .unwrap_err();

    assert!(error.to_string().contains("symlink"));
    assert!(!workspace.path().join("link").exists());
}

#[tokio::test]
async fn an_explicit_sandbox_can_have_an_empty_input_root() {
    let workspace = TempDir::new().unwrap();
    let cache_root = TempDir::new().unwrap();

    let executor = build_executor(workspace.path(), cache_root.path());
    let task = sandboxed("true", &[], &[]);

    let result = executor.execute_task("no-inputs", &task).await.unwrap();
    assert!(result.success);
}

#[tokio::test]
async fn a_sandboxed_task_still_caches() {
    let workspace = TempDir::new().unwrap();
    let cache_root = TempDir::new().unwrap();
    fs::write(workspace.path().join("src.txt"), "source").unwrap();

    let executor = build_executor(workspace.path(), cache_root.path());
    let task = sandboxed("mkdir -p out && cp src.txt out/built.txt", &["src.txt"], &[
        "out/built.txt",
    ]);

    executor.execute_task("build", &task).await.unwrap();
    fs::remove_file(workspace.path().join("out/built.txt")).unwrap();

    let second = executor.execute_task("build", &task).await.unwrap();
    assert!(second.success);
    assert!(
        workspace.path().join("out/built.txt").exists(),
        "the second run must be served from cache, restoring the output"
    );
}

#[tokio::test]
async fn a_plain_hermetic_task_is_sandboxed_without_asking() {
    // The default is the whole point: `inputs` that are only enforced when
    // someone opts in are not a declaration, they are a comment. Bazel and
    // buck2 sandbox by default and this must too.
    let workspace = TempDir::new().unwrap();
    let cache_root = TempDir::new().unwrap();
    fs::write(workspace.path().join("declared.txt"), "declared").unwrap();
    fs::write(workspace.path().join("undeclared.txt"), "undeclared").unwrap();

    let executor = build_executor(workspace.path(), cache_root.path());
    let task = defaulted("cat undeclared.txt", &["declared.txt"], &[]);

    let result = executor.execute_task("peek", &task).await.unwrap();
    assert_ne!(
        result.exit_code,
        Some(0),
        "an undeclared read must fail without the task opting into isolation"
    );
}

#[tokio::test]
async fn the_default_uses_an_empty_input_root_when_no_inputs_are_declared() {
    let workspace = TempDir::new().unwrap();
    let cache_root = TempDir::new().unwrap();
    fs::write(workspace.path().join("ambient.txt"), "ambient").unwrap();

    let executor = build_executor(workspace.path(), cache_root.path());
    let task = defaulted("cat ambient.txt", &[], &[]);

    let result = executor.execute_task("no-inputs", &task).await.unwrap();
    assert_ne!(
        result.exit_code,
        Some(0),
        "an empty declaration must not expose the ambient workspace"
    );
}

#[tokio::test]
async fn sandboxing_is_independent_of_cache_mode() {
    let workspace = TempDir::new().unwrap();
    let cache_root = TempDir::new().unwrap();
    fs::write(workspace.path().join("declared.txt"), "declared").unwrap();
    fs::write(workspace.path().join("undeclared.txt"), "undeclared").unwrap();

    let executor = build_executor(workspace.path(), cache_root.path());
    let mut task = sandboxed("cat undeclared.txt", &["declared.txt"], &[]);
    task.cache = Some(TaskCachePolicy {
        mode: TaskCacheMode::Never,
        max_age: None,
    });

    let result = executor.execute_task("never-cache", &task).await.unwrap();
    assert!(!result.success);
}

#[tokio::test]
async fn nested_task_directory_is_preserved_inside_the_exec_root() {
    let workspace = TempDir::new().unwrap();
    let cache_root = TempDir::new().unwrap();
    fs::create_dir_all(workspace.path().join("sub")).unwrap();
    fs::write(workspace.path().join("sub/input.txt"), "nested").unwrap();

    let executor = build_executor(workspace.path(), cache_root.path());
    let mut task = sandboxed("cat input.txt > output.txt", &["sub/input.txt"], &[
        "output.txt",
    ]);
    task.directory = Some(TaskDirectory {
        from: TaskDirectoryBase::Module,
        path: "sub".to_string(),
    });

    let result = executor.execute_task("nested", &task).await.unwrap();
    assert!(result.success, "stderr: {}", result.stderr);
    assert_eq!(
        fs::read_to_string(workspace.path().join("sub/output.txt")).unwrap(),
        "nested"
    );
}

#[tokio::test]
async fn isolation_can_be_opted_out_of() {
    // The escape hatch for tasks that must touch the live checkout.
    let workspace = TempDir::new().unwrap();
    let cache_root = TempDir::new().unwrap();
    fs::write(workspace.path().join("declared.txt"), "declared").unwrap();
    fs::write(workspace.path().join("undeclared.txt"), "undeclared").unwrap();

    let executor = build_executor(workspace.path(), cache_root.path());
    let task = Task {
        hermetic: Hermetic::Options(HermeticOptions {
            passthrough: Vec::new(),
            sandbox: Some(Sandbox::None),
        }),
        ..sandboxed("cat undeclared.txt", &["declared.txt"], &[])
    };

    let result = executor.execute_task("unsandboxed", &task).await.unwrap();
    assert_eq!(
        result.exit_code,
        Some(0),
        "sandbox: \"none\" must run in the project directory"
    );
}
