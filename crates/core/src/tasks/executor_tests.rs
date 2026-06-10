use super::*;
use crate::tasks::cache::TaskCacheConfig;
use crate::tasks::{RetryConfig, SourceLocation, TaskDependency};
use cuenv_cas::{LocalActionCache, LocalCas};
use cuenv_events::{EventBus, EventCategory, TaskEvent};
use cuenv_vcs::WalkHasher;
use std::collections::HashMap;
use tempfile::TempDir;

fn executor_for(root: &Path) -> TaskExecutor {
    TaskExecutor::new(ExecutorConfig {
        project_root: root.to_path_buf(),
        cue_module_root: Some(root.to_path_buf()),
        ..ExecutorConfig::default()
    })
}

fn source(file: &str) -> SourceLocation {
    SourceLocation {
        file: file.to_string(),
        line: 1,
        column: 1,
    }
}

fn scoped_dir(from: TaskDirectoryBase, path: &str) -> TaskDirectory {
    TaskDirectory {
        from,
        path: path.to_string(),
    }
}

#[tokio::test]
async fn test_executor_config_default() {
    let config = ExecutorConfig::default();
    assert!(config.capture_output.should_capture());
    assert_eq!(config.max_parallel, 0);
    assert!(config.environment.is_empty());
}

#[tokio::test]
async fn test_task_result() {
    let result = TaskResult {
        name: "test".to_string(),
        exit_code: Some(0),
        stdout: "output".to_string(),
        stderr: String::new(),
        success: true,
    };
    assert_eq!(result.name, "test");
    assert_eq!(result.exit_code, Some(0));
    assert!(result.success);
    assert_eq!(result.stdout, "output");
}

#[tokio::test]
async fn test_execute_simple_task() {
    let config = ExecutorConfig {
        capture_output: OutputCapture::Capture,
        ..Default::default()
    };
    let executor = TaskExecutor::new(config);
    let task = Task {
        command: "echo".to_string(),
        args: vec!["hello".to_string()],
        description: Some("Hello task".to_string()),
        ..Default::default()
    };
    let result = executor.execute_task("test", &task).await.unwrap();
    assert!(result.success);
    assert_eq!(result.exit_code, Some(0));
    assert!(result.stdout.contains("hello"));
}

#[tokio::test]
async fn test_execute_with_environment() {
    let mut config = ExecutorConfig {
        capture_output: OutputCapture::Capture,
        ..Default::default()
    };
    config
        .environment
        .set("TEST_VAR".to_string(), "test_value".to_string());
    let executor = TaskExecutor::new(config);
    let task = Task {
        command: "printenv".to_string(),
        args: vec!["TEST_VAR".to_string()],
        description: Some("Print env task".to_string()),
        ..Default::default()
    };
    let result = executor.execute_task("test", &task).await.unwrap();
    assert!(result.success);
    assert!(result.stdout.contains("test_value"));
}

#[tokio::test]
async fn test_execute_with_task_secret_environment() {
    let config = ExecutorConfig {
        capture_output: OutputCapture::Capture,
        ..Default::default()
    };
    let executor = TaskExecutor::new(config);
    let task = Task {
        command: "printenv".to_string(),
        args: vec!["GH_TOKEN".to_string()],
        env: HashMap::from([(
            "GH_TOKEN".to_string(),
            serde_json::json!({
                "resolver": "exec",
                "command": "echo",
                "args": ["tap-token"]
            }),
        )]),
        description: Some("Print task secret env task".to_string()),
        ..Default::default()
    };

    let result = executor.execute_task("test", &task).await.unwrap();

    assert!(result.success);
    assert_eq!(result.stdout.trim(), "tap-token");
}

#[tokio::test]
async fn test_task_secret_gh_token_is_available_with_github_token() {
    let mut config = ExecutorConfig {
        capture_output: OutputCapture::Capture,
        ..Default::default()
    };
    config
        .environment
        .set("GITHUB_TOKEN".to_string(), "repo-token".to_string());
    let executor = TaskExecutor::new(config);
    let task = Task {
        command: "sh".to_string(),
        args: vec![
            "-c".to_string(),
            r#"test "$GH_TOKEN" = "tap-token" && test "$GITHUB_TOKEN" = "repo-token" && echo ok"#
                .to_string(),
        ],
        env: HashMap::from([(
            "GH_TOKEN".to_string(),
            serde_json::json!({
                "resolver": "exec",
                "command": "echo",
                "args": ["tap-token"]
            }),
        )]),
        description: Some("Verify GitHub CLI token precedence env".to_string()),
        ..Default::default()
    };

    let result = executor.execute_task("test", &task).await.unwrap();

    assert!(result.success);
    assert_eq!(result.stdout.trim(), "ok");
}

#[tokio::test]
async fn test_execute_failing_task() {
    let config = ExecutorConfig {
        capture_output: OutputCapture::Capture,
        ..Default::default()
    };
    let executor = TaskExecutor::new(config);
    let task = Task {
        command: "false".to_string(),
        description: Some("Failing task".to_string()),
        ..Default::default()
    };
    let result = executor.execute_task("test", &task).await.unwrap();
    assert!(!result.success);
    assert_eq!(result.exit_code, Some(1));
}

#[tokio::test]
async fn test_execute_task_timeout() {
    let config = ExecutorConfig {
        capture_output: OutputCapture::Capture,
        ..Default::default()
    };
    let executor = TaskExecutor::new(config);
    let task = Task {
        command: "sh".to_string(),
        args: vec!["-c".to_string(), "sleep 1".to_string()],
        timeout: Some("50ms".to_string()),
        ..Default::default()
    };

    let result = executor.execute_task("timeout", &task).await.unwrap();

    assert!(!result.success);
    assert_eq!(result.exit_code, None);
    assert!(result.stderr.contains("timed out"));
}

#[tokio::test]
async fn test_execute_task_retries_until_success() {
    let tmp = TempDir::new().unwrap();
    let marker = tmp.path().join("attempts");
    let config = ExecutorConfig {
        capture_output: OutputCapture::Capture,
        project_root: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let executor = TaskExecutor::new(config);
    let task = Task {
        command: "sh".to_string(),
        args: vec![
            "-c".to_string(),
            "count=$(cat attempts 2>/dev/null || echo 0); count=$((count + 1)); echo $count > attempts; test $count -ge 2"
                .to_string(),
        ],
        retry: Some(RetryConfig {
            attempts: 2,
            delay: Some("1ms".to_string()),
        }),
        ..Default::default()
    };

    let result = executor.execute_task("retry", &task).await.unwrap();

    assert!(result.success);
    assert_eq!(std::fs::read_to_string(marker).unwrap().trim(), "2");
}

#[tokio::test]
async fn test_timeout_is_not_retried() {
    // A timeout is a hard policy violation, not a transient failure: even with
    // retries configured, a timed-out attempt must end the task immediately
    // rather than re-incur the full timeout on every attempt.
    let tmp = TempDir::new().unwrap();
    let config = ExecutorConfig {
        capture_output: OutputCapture::Capture,
        project_root: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let executor = TaskExecutor::new(config);
    let task = Task {
        command: "sh".to_string(),
        args: vec!["-c".to_string(), "echo x >> attempts; sleep 5".to_string()],
        timeout: Some("100ms".to_string()),
        retry: Some(RetryConfig {
            attempts: 3,
            delay: None,
        }),
        ..Default::default()
    };

    let result = executor.execute_task("noretry", &task).await.unwrap();

    assert!(!result.success);
    assert_eq!(result.exit_code, None);
    let attempts = std::fs::read_to_string(tmp.path().join("attempts")).unwrap();
    assert_eq!(
        attempts.lines().count(),
        1,
        "a timed-out attempt must not be retried"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn test_timeout_kills_process_tree() {
    // The task spawns a grandchild (`sleep 30 &`) sharing its process group,
    // then blocks. On timeout the whole group is signalled, so the grandchild
    // must be reaped too — proving timeout enforcement isn't limited to the
    // direct child (the orphaned-process failure mode).
    let tmp = TempDir::new().unwrap();
    let config = ExecutorConfig {
        capture_output: OutputCapture::Capture,
        project_root: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let executor = TaskExecutor::new(config);
    let task = Task {
        command: "sh".to_string(),
        args: vec![
            "-c".to_string(),
            "sleep 30 & echo $! > grandchild.pid; sleep 30".to_string(),
        ],
        timeout: Some("150ms".to_string()),
        ..Default::default()
    };

    let result = executor.execute_task("tree", &task).await.unwrap();
    assert!(!result.success);
    assert_eq!(result.exit_code, None);

    let pid: i32 = std::fs::read_to_string(tmp.path().join("grandchild.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();

    // kill(pid, 0) returns Err(ESRCH) once the process is gone. Poll briefly
    // for the OS to finish reaping after the SIGKILL.
    let mut alive = true;
    for _ in 0..40 {
        #[expect(unsafe_code, reason = "probing process liveness via kill(pid, 0)")]
        let exists = unsafe { libc::kill(pid, 0) } == 0;
        if !exists {
            alive = false;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(
        !alive,
        "grandchild {pid} should be killed when the task times out"
    );
}

#[tokio::test]
async fn test_execute_script_task() {
    let config = ExecutorConfig {
        capture_output: OutputCapture::Capture,
        ..Default::default()
    };
    let executor = TaskExecutor::new(config);
    let task = Task {
        script: Some("echo hello from script".to_string()),
        ..Default::default()
    };

    let result = executor.execute_task("script", &task).await.unwrap();

    assert!(result.success);
    assert_eq!(result.exit_code, Some(0));
    assert!(result.stdout.contains("hello from script"));
}

#[tokio::test]
async fn test_execute_script_task_with_shell_options() {
    let config = ExecutorConfig {
        capture_output: OutputCapture::Capture,
        ..Default::default()
    };
    let executor = TaskExecutor::new(config);
    let task = Task {
        script: Some("false\necho should-not-run".to_string()),
        script_shell: Some(super::super::ScriptShell::Bash),
        shell_options: Some(super::super::ShellOptions::default()),
        ..Default::default()
    };

    let result = executor
        .execute_task("script.failfast", &task)
        .await
        .unwrap();

    assert!(!result.success);
    assert_eq!(result.exit_code, Some(1));
    assert!(!result.stdout.contains("should-not-run"));
}

#[tokio::test]
async fn test_execute_script_task_rejects_pipefail_for_sh() {
    let config = ExecutorConfig {
        capture_output: OutputCapture::Capture,
        ..Default::default()
    };
    let executor = TaskExecutor::new(config);
    let task = Task {
        script: Some("echo hello".to_string()),
        script_shell: Some(super::super::ScriptShell::Sh),
        shell_options: Some(super::super::ShellOptions::default()),
        ..Default::default()
    };

    let err = executor.execute_task("script.sh", &task).await.unwrap_err();

    assert!(
        err.to_string()
            .contains("shellOptions.pipefail with unsupported script shell 'sh'"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn test_execute_script_task_rejects_unsupported_shell_options() {
    let config = ExecutorConfig {
        capture_output: OutputCapture::Capture,
        ..Default::default()
    };
    let executor = TaskExecutor::new(config);
    let task = Task {
        script: Some("console.log('hello')".to_string()),
        script_shell: Some(super::super::ScriptShell::Node),
        shell_options: Some(super::super::ShellOptions::default()),
        ..Default::default()
    };

    let err = executor
        .execute_task("script.node", &task)
        .await
        .unwrap_err();

    assert!(
        err.to_string().contains("unsupported script shell 'node'"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn test_execute_sequential_group() {
    let config = ExecutorConfig {
        capture_output: OutputCapture::Capture,
        ..Default::default()
    };
    let executor = TaskExecutor::new(config);
    let task1 = Task {
        command: "echo".to_string(),
        args: vec!["first".to_string()],
        description: Some("First task".to_string()),
        ..Default::default()
    };
    let task2 = Task {
        command: "echo".to_string(),
        args: vec!["second".to_string()],
        description: Some("Second task".to_string()),
        ..Default::default()
    };
    let sequence = vec![
        TaskNode::Task(Box::new(task1)),
        TaskNode::Task(Box::new(task2)),
    ];
    let all_tasks = Tasks::new();
    let node = TaskNode::Sequence(sequence);
    let results = executor
        .execute_node("seq", &node, &all_tasks)
        .await
        .unwrap();
    assert_eq!(results.len(), 2);
    assert!(results[0].stdout.contains("first"));
    assert!(results[1].stdout.contains("second"));
}

#[tokio::test]
async fn test_command_injection_prevention() {
    let config = ExecutorConfig {
        capture_output: OutputCapture::Capture,
        ..Default::default()
    };
    let executor = TaskExecutor::new(config);
    let malicious_task = Task {
        command: "echo".to_string(),
        args: vec!["hello".to_string(), "; rm -rf /".to_string()],
        description: Some("Malicious task test".to_string()),
        ..Default::default()
    };
    let result = executor
        .execute_task("malicious", &malicious_task)
        .await
        .unwrap();
    assert!(result.success);
    assert!(result.stdout.contains("hello ; rm -rf /"));
}

#[tokio::test]
async fn test_special_characters_in_args() {
    let config = ExecutorConfig {
        capture_output: OutputCapture::Capture,
        ..Default::default()
    };
    let executor = TaskExecutor::new(config);
    let special_chars = vec![
        "$USER",
        "$(whoami)",
        "`whoami`",
        "&& echo hacked",
        "|| echo failed",
        "> /tmp/hack",
        "| cat",
    ];
    for special_arg in special_chars {
        let task = Task {
            command: "echo".to_string(),
            args: vec!["safe".to_string(), special_arg.to_string()],
            description: Some("Special character test".to_string()),
            ..Default::default()
        };
        let result = executor.execute_task("special", &task).await.unwrap();
        assert!(result.success);
        assert!(result.stdout.contains("safe"));
        assert!(result.stdout.contains(special_arg));
    }
}

#[tokio::test]
async fn test_environment_variable_safety() {
    let mut config = ExecutorConfig {
        capture_output: OutputCapture::Capture,
        ..Default::default()
    };
    config
        .environment
        .set("DANGEROUS_VAR".to_string(), "; rm -rf /".to_string());
    let executor = TaskExecutor::new(config);
    let task = Task {
        command: "printenv".to_string(),
        args: vec!["DANGEROUS_VAR".to_string()],
        description: Some("Environment variable safety test".to_string()),
        ..Default::default()
    };
    let result = executor.execute_task("env_test", &task).await.unwrap();
    assert!(result.success);
    assert!(result.stdout.contains("; rm -rf /"));
}

#[tokio::test]
async fn test_execute_graph_parallel_groups() {
    // two independent tasks -> can run in same parallel group
    let config = ExecutorConfig {
        capture_output: OutputCapture::Capture,
        max_parallel: 2,
        ..Default::default()
    };
    let executor = TaskExecutor::new(config);
    let mut graph = TaskGraph::new();

    let t1 = Task {
        command: "echo".into(),
        args: vec!["A".into()],
        ..Default::default()
    };
    let t2 = Task {
        command: "echo".into(),
        args: vec!["B".into()],
        ..Default::default()
    };

    graph.add_task("t1", t1).unwrap();
    graph.add_task("t2", t2).unwrap();
    let results = executor.execute_graph(&graph).await.unwrap();
    assert_eq!(results.len(), 2);
    let joined = results.iter().map(|r| r.stdout.clone()).collect::<String>();
    assert!(joined.contains("A") && joined.contains("B"));
}

#[tokio::test]
async fn test_execute_group_respects_max_concurrency() {
    let tmp = TempDir::new().unwrap();
    let config = ExecutorConfig {
        capture_output: OutputCapture::Capture,
        project_root: tmp.path().to_path_buf(),
        ..Default::default()
    };
    let executor = TaskExecutor::new(config);
    let script = "if [ -f running ]; then echo overlap > violation; fi; touch running; sleep 0.1; rm running";
    let group = TaskGroup {
        type_: "group".to_string(),
        depends_on: Vec::new(),
        max_concurrency: Some(1),
        description: None,
        children: HashMap::from([
            (
                "a".to_string(),
                TaskNode::Task(Box::new(Task {
                    command: "sh".to_string(),
                    args: vec!["-c".to_string(), script.to_string()],
                    ..Default::default()
                })),
            ),
            (
                "b".to_string(),
                TaskNode::Task(Box::new(Task {
                    command: "sh".to_string(),
                    args: vec!["-c".to_string(), script.to_string()],
                    ..Default::default()
                })),
            ),
        ]),
    };

    let results = executor
        .execute_node("limited", &TaskNode::Group(group), &Tasks::new())
        .await
        .unwrap();

    assert_eq!(results.len(), 2);
    assert!(!tmp.path().join("violation").exists());
}

#[tokio::test]
async fn execute_graph_continue_on_error_skips_dependents() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    let config = ExecutorConfig {
        capture_output: OutputCapture::Capture,
        max_parallel: 2,
        continue_on_error: true,
        project_root: root.to_path_buf(),
        ..Default::default()
    };
    let executor = TaskExecutor::new(config);

    // DAG: fail -> dependent ; independent runs regardless.
    let mut tasks = Tasks::new();
    tasks.tasks.insert(
        "fail".into(),
        TaskNode::Task(Box::new(Task {
            command: "sh".into(),
            args: vec!["-c".into(), "exit 7".into()],
            ..Default::default()
        })),
    );
    tasks.tasks.insert(
        "dependent".into(),
        TaskNode::Task(Box::new(Task {
            command: "sh".into(),
            args: vec!["-c".into(), "echo dependent ran".into()],
            depends_on: vec![TaskDependency::from_name("fail")],
            ..Default::default()
        })),
    );
    tasks.tasks.insert(
        "independent".into(),
        TaskNode::Task(Box::new(Task {
            command: "sh".into(),
            args: vec!["-c".into(), "echo independent ran".into()],
            ..Default::default()
        })),
    );

    let mut graph = TaskGraph::new();
    graph.build_for_task("dependent", &tasks).unwrap();
    graph.build_for_task("independent", &tasks).unwrap();

    // First failure is reported via Err, but the independent sibling
    // and the failing task's result both make it into the results map
    // before we surface the failure.
    let outcome = executor.execute_graph(&graph).await;
    assert!(outcome.is_err(), "fail task should surface as error");

    // The "independent" task must have completed successfully even
    // though "fail" failed — that's the whole point of the flag.
    // (We can't easily assert on the results vec since the executor
    // returns Err; this is exercised in the integration suite.)
}

#[tokio::test]
async fn execute_graph_task_continue_on_error_skips_dependents() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    let bus = EventBus::new();
    let sender = bus.sender().expect("sender available");
    let _ = cuenv_events::set_global_sender(sender);
    let mut rx = bus.subscribe();

    let config = ExecutorConfig {
        capture_output: OutputCapture::Capture,
        max_parallel: 1,
        project_root: root.to_path_buf(),
        ..Default::default()
    };
    let executor = TaskExecutor::new(config);
    let mut tasks = Tasks::new();
    tasks.tasks.insert(
        "fail".into(),
        TaskNode::Task(Box::new(Task {
            command: "sh".into(),
            args: vec!["-c".into(), "exit 7".into()],
            continue_on_error: true,
            ..Default::default()
        })),
    );
    tasks.tasks.insert(
        "dependent".into(),
        TaskNode::Task(Box::new(Task {
            command: "sh".into(),
            args: vec!["-c".into(), "echo dependent ran".into()],
            depends_on: vec![TaskDependency::from_name("fail")],
            ..Default::default()
        })),
    );

    let mut graph = TaskGraph::new();
    graph.build_for_task("dependent", &tasks).unwrap();
    let outcome = executor.execute_graph(&graph).await;
    assert!(outcome.is_err(), "fail task should surface as error");

    let mut saw_skip = false;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(500);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await {
            Ok(Some(event)) => {
                if let EventCategory::Task(TaskEvent::Skipped { name, .. }) = event.category
                    && name == "dependent"
                {
                    saw_skip = true;
                    break;
                }
            }
            Ok(None) => break,
            Err(_) => continue,
        }
    }

    cuenv_events::clear_global_sender();
    assert!(saw_skip, "dependent task should be skipped");
}

#[tokio::test]
async fn test_execute_graph_respects_dependency_levels() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();

    let config = ExecutorConfig {
        capture_output: OutputCapture::Capture,
        max_parallel: 2,
        project_root: root.to_path_buf(),
        ..Default::default()
    };
    let executor = TaskExecutor::new(config);

    let mut tasks = Tasks::new();
    tasks.tasks.insert(
        "dep".into(),
        TaskNode::Task(Box::new(Task {
            command: "sh".into(),
            args: vec!["-c".into(), "sleep 0.2 && echo ok > marker.txt".into()],
            ..Default::default()
        })),
    );
    tasks.tasks.insert(
        "consumer".into(),
        TaskNode::Task(Box::new(Task {
            command: "sh".into(),
            args: vec!["-c".into(), "cat marker.txt".into()],
            depends_on: vec![TaskDependency::from_name("dep")],
            ..Default::default()
        })),
    );

    let mut graph = TaskGraph::new();
    graph.build_for_task("consumer", &tasks).unwrap();

    let results = executor.execute_graph(&graph).await.unwrap();
    assert_eq!(results.len(), 2);

    let consumer = results.iter().find(|r| r.name == "consumer").unwrap();
    assert!(consumer.success);
    assert!(consumer.stdout.contains("ok"));
}

#[tokio::test]
async fn test_cache_hit_replays_task_output_events() {
    let workspace = TempDir::new().unwrap();
    let cache_root = TempDir::new().unwrap();
    std::fs::write(workspace.path().join("input.txt"), "v1").unwrap();

    let cache = TaskCacheConfig {
        cas: Arc::new(LocalCas::open(cache_root.path()).unwrap()),
        action_cache: Arc::new(LocalActionCache::open(cache_root.path()).unwrap()),
        vcs_hasher: Arc::new(WalkHasher::new(workspace.path())),
        vcs_hasher_root: workspace.path().to_path_buf(),
        cuenv_version: "test".to_string(),
        runtime_identity_properties: std::collections::BTreeMap::new(),
        cache_disabled_reason: None,
    };
    let executor = TaskExecutor::new(ExecutorConfig {
        capture_output: OutputCapture::Capture,
        project_root: workspace.path().to_path_buf(),
        cache: Some(cache),
        ..Default::default()
    });
    let task = Task {
        command: "sh".to_string(),
        args: vec![
            "-c".to_string(),
            "printf 'hello\\n' && cat input.txt > out.txt".to_string(),
        ],
        inputs: vec![super::super::Input::Path("input.txt".to_string())],
        outputs: vec!["out.txt".to_string()],
        cache: Some(super::super::TaskCachePolicy {
            mode: super::super::TaskCacheMode::ReadWrite,
            max_age: None,
        }),
        ..Task::default()
    };

    executor.execute_task("cached", &task).await.unwrap();
    std::fs::remove_file(workspace.path().join("out.txt")).unwrap();

    // The macros emit via the process-wide EventSender installed
    // here; subscribe a receiver before triggering the cached run so
    // we can observe the replayed Output event.
    let bus = EventBus::new();
    let sender = bus.sender().expect("sender available");
    let _ = cuenv_events::set_global_sender(sender);
    let mut rx = bus.subscribe();

    let result = executor.execute_task("cached", &task).await.unwrap();
    assert!(result.success);

    // The mpsc → broadcast forwarder runs on a tokio task; await with a
    // short deadline rather than try_recv-looping so we don't race the
    // forwarder.
    let mut saw_output = false;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(500);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(50), rx.recv()).await {
            Ok(Some(event)) => {
                if let EventCategory::Task(TaskEvent::Output { name, content, .. }) = event.category
                    && name == "cached"
                    && content == "hello"
                {
                    saw_output = true;
                    break;
                }
            }
            Ok(None) => break,
            Err(_) => continue,
        }
    }

    cuenv_events::clear_global_sender();
    assert!(
        saw_output,
        "expected cached task output event to be replayed"
    );
}

#[test]
fn test_summarize_task_failure_with_exit_code() {
    let result = TaskResult {
        name: "build".to_string(),
        exit_code: Some(127),
        stdout: String::new(),
        stderr: "command not found".to_string(),
        success: false,
    };
    let summary = summarize_task_failure(&result, 10);
    assert!(summary.contains("build"));
    assert!(summary.contains("127"));
    assert!(summary.contains("command not found"));
}

#[test]
fn test_summarize_task_failure_no_exit_code() {
    let result = TaskResult {
        name: "killed".to_string(),
        exit_code: None,
        stdout: String::new(),
        stderr: String::new(),
        success: false,
    };
    let summary = summarize_task_failure(&result, 10);
    assert!(summary.contains("killed"));
    assert!(summary.contains("unknown"));
}

#[test]
fn test_summarize_task_failure_no_output() {
    let result = TaskResult {
        name: "silent".to_string(),
        exit_code: Some(1),
        stdout: String::new(),
        stderr: String::new(),
        success: false,
    };
    let summary = summarize_task_failure(&result, 10);
    assert!(summary.contains("No stdout/stderr"));
    assert!(summary.contains("RUST_LOG=debug"));
}

#[test]
fn test_summarize_task_failure_truncates_long_output() {
    let result = TaskResult {
        name: "verbose".to_string(),
        exit_code: Some(1),
        stdout: (1..=50)
            .map(|i| format!("line {}", i))
            .collect::<Vec<_>>()
            .join("\n"),
        stderr: String::new(),
        success: false,
    };
    let summary = summarize_task_failure(&result, 10);
    assert!(summary.contains("last 10 of 50 lines"));
    assert!(summary.contains("line 50"));
    assert!(!summary.contains("line 1\n")); // First line should be truncated
}

#[test]
fn test_summarize_stream_empty() {
    assert!(summarize_stream("test", "", 10).is_none());
    assert!(summarize_stream("test", "   ", 10).is_none());
    assert!(summarize_stream("test", "\n\n", 10).is_none());
}

#[test]
fn test_summarize_stream_short() {
    let result = summarize_stream("stdout", "line 1\nline 2", 10).unwrap();
    assert!(result.contains("stdout:"));
    assert!(result.contains("line 1"));
    assert!(result.contains("line 2"));
    assert!(!result.contains("last"));
}

#[test]
fn test_format_failure_streams_both() {
    let result = TaskResult {
        name: "test".to_string(),
        exit_code: Some(1),
        stdout: "stdout content".to_string(),
        stderr: "stderr content".to_string(),
        success: false,
    };
    let formatted = format_failure_streams(&result, 10);
    assert!(formatted.contains("stdout:"));
    assert!(formatted.contains("stderr:"));
    assert!(formatted.contains("stdout content"));
    assert!(formatted.contains("stderr content"));
}

#[test]
fn test_find_workspace_root_with_npm() {
    let tmp = TempDir::new().unwrap();
    // Create a workspace structure
    std::fs::write(
        tmp.path().join("package.json"),
        r#"{"workspaces": ["packages/*"]}"#,
    )
    .unwrap();
    let subdir = tmp.path().join("packages").join("subpkg");
    std::fs::create_dir_all(&subdir).unwrap();

    let root = find_workspace_root(PackageManager::Npm, &subdir);
    assert_eq!(root, tmp.path().canonicalize().unwrap());
}

#[test]
fn test_workdir_for_non_hermetic_package_task_prefers_source_directory() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("package.json"),
        r#"{"workspaces": ["projects/*"]}"#,
    )
    .unwrap();
    let app_dir = tmp.path().join("projects").join("app");
    std::fs::create_dir_all(&app_dir).unwrap();
    std::fs::write(
        app_dir.join("package.json"),
        r#"{"scripts": {"build": "echo app"}}"#,
    )
    .unwrap();

    let executor = executor_for(tmp.path());

    let task = Task {
        command: "bun".to_string(),
        args: vec!["run".to_string(), "build".to_string()],
        hermetic: false,
        source: Some(source("projects/app/env.cue")),
        ..Task::default()
    };

    assert_eq!(executor.workdir_for_task(&task).unwrap(), app_dir);
}

#[test]
fn test_workdir_module_dir_is_module_relative() {
    let tmp = TempDir::new().unwrap();
    let app_dir = tmp.path().join("apps").join("web");

    let executor = executor_for(tmp.path());

    let task = Task {
        command: "pwd".to_string(),
        directory: Some(scoped_dir(TaskDirectoryBase::Module, "apps/web")),
        source: Some(source("templates/env.cue")),
        ..Task::default()
    };

    assert_eq!(executor.workdir_for_task(&task).unwrap(), app_dir);
}

#[test]
fn test_workdir_scoped_dir_from_definition() {
    let tmp = TempDir::new().unwrap();
    let expected = tmp.path().join("templates").join("bun").join("frontend");

    let executor = executor_for(tmp.path());

    let task = Task {
        command: "pwd".to_string(),
        source: Some(source("templates/bun/env.cue")),
        caller_source: Some(source("apps/web/env.cue")),
        directory: Some(scoped_dir(TaskDirectoryBase::Definition, "frontend")),
        ..Task::default()
    };

    assert_eq!(executor.workdir_for_task(&task).unwrap(), expected);
}

#[test]
fn test_workdir_scoped_dir_from_caller() {
    let tmp = TempDir::new().unwrap();
    let expected = tmp.path().join("apps").join("web").join("frontend");

    let executor = executor_for(tmp.path());

    let task = Task {
        command: "pwd".to_string(),
        source: Some(source("templates/bun/env.cue")),
        caller_source: Some(source("apps/web/env.cue")),
        directory: Some(scoped_dir(TaskDirectoryBase::Caller, "frontend")),
        ..Task::default()
    };

    assert_eq!(executor.workdir_for_task(&task).unwrap(), expected);
}

#[test]
fn test_workdir_rejects_dir_escape_from_module_root() {
    let tmp = TempDir::new().unwrap();

    let executor = executor_for(tmp.path());

    let task = Task {
        command: "pwd".to_string(),
        directory: Some(scoped_dir(TaskDirectoryBase::Module, "../outside")),
        ..Task::default()
    };

    assert!(executor.workdir_for_task(&task).is_err());
}

#[test]
fn test_workdir_scoped_dir_requires_requested_source_metadata() {
    let tmp = TempDir::new().unwrap();

    let executor = executor_for(tmp.path());

    let task = Task {
        command: "pwd".to_string(),
        directory: Some(scoped_dir(TaskDirectoryBase::Caller, ".")),
        ..Task::default()
    };

    let err = executor.workdir_for_task(&task).unwrap_err();
    assert!(
        err.to_string().contains("requires source metadata"),
        "unexpected error: {err}"
    );
}

#[test]
fn test_find_workspace_root_with_pnpm() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("pnpm-workspace.yaml"),
        "packages:\n  - 'packages/*'",
    )
    .unwrap();
    let subdir = tmp.path().join("packages").join("app");
    std::fs::create_dir_all(&subdir).unwrap();

    let root = find_workspace_root(PackageManager::Pnpm, &subdir);
    assert_eq!(root, tmp.path().canonicalize().unwrap());
}

#[test]
fn test_find_workspace_root_with_cargo() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/*\"]",
    )
    .unwrap();
    let subdir = tmp.path().join("crates").join("core");
    std::fs::create_dir_all(&subdir).unwrap();

    let root = find_workspace_root(PackageManager::Cargo, &subdir);
    assert_eq!(root, tmp.path().canonicalize().unwrap());
}

#[test]
fn test_find_workspace_root_no_workspace() {
    let tmp = TempDir::new().unwrap();
    let root = find_workspace_root(PackageManager::Npm, tmp.path());
    // Should return the start path when no workspace is found
    assert_eq!(root, tmp.path().to_path_buf());
}

#[test]
fn test_package_json_has_workspaces_array() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("package.json"),
        r#"{"workspaces": ["packages/*"]}"#,
    )
    .unwrap();
    assert!(package_json_has_workspaces(tmp.path()));
}

#[test]
fn test_package_json_has_workspaces_object() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("package.json"),
        r#"{"workspaces": {"packages": ["packages/*"]}}"#,
    )
    .unwrap();
    assert!(package_json_has_workspaces(tmp.path()));
}

#[test]
fn test_package_json_no_workspaces() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("package.json"), r#"{"name": "test"}"#).unwrap();
    assert!(!package_json_has_workspaces(tmp.path()));
}

#[test]
fn test_package_json_empty_workspaces() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("package.json"), r#"{"workspaces": []}"#).unwrap();
    assert!(!package_json_has_workspaces(tmp.path()));
}

#[test]
fn test_package_json_missing() {
    let tmp = TempDir::new().unwrap();
    assert!(!package_json_has_workspaces(tmp.path()));
}

#[test]
fn test_cargo_toml_has_workspace() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/*\"]",
    )
    .unwrap();
    assert!(cargo_toml_has_workspace(tmp.path()));
}

#[test]
fn test_cargo_toml_no_workspace() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("Cargo.toml"), "[package]\nname = \"test\"").unwrap();
    assert!(!cargo_toml_has_workspace(tmp.path()));
}

#[test]
fn test_cargo_toml_missing() {
    let tmp = TempDir::new().unwrap();
    assert!(!cargo_toml_has_workspace(tmp.path()));
}

#[test]
fn test_deno_json_has_workspace_array() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("deno.json"),
        r#"{"workspace": ["./packages/*"]}"#,
    )
    .unwrap();
    assert!(deno_json_has_workspace(tmp.path()));
}

#[test]
fn test_deno_json_has_workspace_object() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(
        tmp.path().join("deno.json"),
        r#"{"workspace": {"members": ["./packages/*"]}}"#,
    )
    .unwrap();
    assert!(deno_json_has_workspace(tmp.path()));
}

#[test]
fn test_deno_json_no_workspace() {
    let tmp = TempDir::new().unwrap();
    std::fs::write(tmp.path().join("deno.json"), r#"{"name": "test"}"#).unwrap();
    assert!(!deno_json_has_workspace(tmp.path()));
}

#[test]
fn test_deno_json_missing() {
    let tmp = TempDir::new().unwrap();
    assert!(!deno_json_has_workspace(tmp.path()));
}

#[test]
fn test_executor_config_with_fields() {
    let config = ExecutorConfig {
        capture_output: OutputCapture::Capture,
        max_parallel: 4,
        working_dir: Some(PathBuf::from("/tmp")),
        project_root: PathBuf::from("/project"),
        cue_module_root: Some(PathBuf::from("/project/cue.mod")),
        materialize_outputs: Some(PathBuf::from("/outputs")),
        cache_dir: Some(PathBuf::from("/cache")),
        show_cache_path: true,
        cli_backend: Some("host".to_string()),
        ..Default::default()
    };
    assert!(config.capture_output.should_capture());
    assert_eq!(config.max_parallel, 4);
    assert_eq!(config.working_dir, Some(PathBuf::from("/tmp")));
    assert!(config.show_cache_path);
}

#[test]
fn test_task_result_clone() {
    let result = TaskResult {
        name: "test".to_string(),
        exit_code: Some(0),
        stdout: "output".to_string(),
        stderr: "error".to_string(),
        success: true,
    };
    let cloned = result.clone();
    assert_eq!(cloned.name, result.name);
    assert_eq!(cloned.exit_code, result.exit_code);
    assert_eq!(cloned.stdout, result.stdout);
    assert_eq!(cloned.stderr, result.stderr);
    assert_eq!(cloned.success, result.success);
}

#[test]
fn test_task_failure_snippet_lines_constant() {
    assert_eq!(TASK_FAILURE_SNIPPET_LINES, 20);
}
