use super::*;

#[test]
fn test_event_creation() {
    let event = CuenvEvent::new(
        Uuid::new_v4(),
        EventSource::new("cuenv::test"),
        EventCategory::Output(OutputEvent::Stdout {
            content: "test".to_string(),
        }),
    );

    assert!(!event.id.is_nil());
    assert_eq!(event.source.target, "cuenv::test");
}

#[test]
fn test_event_serialization() {
    let event = CuenvEvent::new(
        Uuid::new_v4(),
        EventSource::new("cuenv::task"),
        EventCategory::Task(TaskEvent::Started {
            name: "build".to_string(),
            command: "cargo build".to_string(),
            hermetic: true,
            parent_group: None,
            task_kind: TaskKind::Task,
        }),
    );

    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("cuenv::task"));
    assert!(json.contains("build"));

    let parsed: CuenvEvent = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.id, event.id);
}

#[test]
fn test_started_backcompat_serde_no_parent_group() {
    let json =
        r#"{"event":"Started","data":{"name":"build","command":"cargo build","hermetic":true}}"#;
    let parsed: TaskEvent = serde_json::from_str(json).unwrap();
    match parsed {
        TaskEvent::Started {
            parent_group,
            task_kind,
            ..
        } => {
            assert_eq!(parent_group, None);
            assert_eq!(task_kind, TaskKind::Task);
        }
        _ => panic!("expected Started"),
    }
}

#[test]
fn test_event_source_with_location() {
    let source = EventSource::with_location("cuenv::task", "src/main.rs", 42);
    assert_eq!(source.target, "cuenv::task");
    assert_eq!(source.file, Some("src/main.rs".to_string()));
    assert_eq!(source.line, Some(42));
}

#[test]
fn test_event_source_new() {
    let source = EventSource::new("cuenv::ci");
    assert_eq!(source.target, "cuenv::ci");
    assert!(source.file.is_none());
    assert!(source.line.is_none());
}

#[test]
fn test_task_event_cache_hit() {
    let event = TaskEvent::CacheHit {
        name: "test".to_string(),
        cache_key: "abc123".to_string(),
        parent_group: Some("ci".to_string()),
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("CacheHit"));
    assert!(json.contains("abc123"));
    assert!(json.contains("\"parent_group\":\"ci\""));
}

#[test]
fn test_task_event_cache_miss() {
    let event = TaskEvent::CacheMiss {
        name: "test".to_string(),
        parent_group: None,
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("CacheMiss"));
    // parent_group: None should not be serialized
    assert!(!json.contains("parent_group"));
}

#[test]
fn test_task_event_cache_skipped() {
    let event = TaskEvent::CacheSkipped {
        name: "fmt".to_string(),
        parent_group: None,
        reason: CacheSkipReason::EmptyInputs,
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("CacheSkipped"));
    assert!(json.contains("empty_inputs"));

    let parsed: TaskEvent = serde_json::from_str(&json).unwrap();
    match parsed {
        TaskEvent::CacheSkipped { reason, .. } => {
            assert_eq!(reason, CacheSkipReason::EmptyInputs);
        }
        _ => panic!("expected CacheSkipped"),
    }
}

#[test]
fn test_task_event_queued() {
    let event = TaskEvent::Queued {
        name: "build".to_string(),
        parent_group: Some("ci".to_string()),
        queue_position: 3,
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("Queued"));
    assert!(json.contains("\"queue_position\":3"));
}

#[test]
fn test_task_event_skipped_dependency_failed() {
    let event = TaskEvent::Skipped {
        name: "deploy".to_string(),
        parent_group: None,
        reason: SkipReason::DependencyFailed {
            dep: "build".to_string(),
        },
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("Skipped"));
    let parsed: TaskEvent = serde_json::from_str(&json).unwrap();
    match parsed {
        TaskEvent::Skipped { reason, .. } => {
            assert_eq!(
                reason,
                SkipReason::DependencyFailed {
                    dep: "build".to_string()
                }
            );
        }
        _ => panic!("expected Skipped"),
    }
}

#[test]
fn test_task_event_retrying() {
    let event = TaskEvent::Retrying {
        name: "flaky".to_string(),
        parent_group: None,
        attempt: 2,
        max_attempts: 3,
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("Retrying"));
    assert!(json.contains("\"attempt\":2"));
    assert!(json.contains("\"max_attempts\":3"));
}

#[test]
fn test_task_event_output() {
    let event = TaskEvent::Output {
        name: "build".to_string(),
        stream: Stream::Stdout,
        content: "compiling...".to_string(),
        parent_group: None,
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("Output"));
    assert!(json.contains("Stdout"));
}

#[test]
fn test_task_event_completed() {
    let event = TaskEvent::Completed {
        name: "build".to_string(),
        success: true,
        exit_code: Some(0),
        duration_ms: 1500,
        parent_group: None,
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("Completed"));
    assert!(json.contains("1500"));
}

#[test]
fn test_task_event_group_started() {
    let event = TaskEvent::GroupStarted {
        name: "tests".to_string(),
        sequential: false,
        task_count: 5,
        parent_group: None,
        max_concurrency: Some(4),
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("GroupStarted"));
    assert!(json.contains('5'));
    assert!(json.contains("\"max_concurrency\":4"));
}

#[test]
fn test_task_event_group_completed() {
    let event = TaskEvent::GroupCompleted {
        name: "tests".to_string(),
        success: true,
        duration_ms: 3000,
        parent_group: None,
        succeeded: 4,
        failed: 1,
        skipped: 0,
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("GroupCompleted"));
    assert!(json.contains("\"succeeded\":4"));
    assert!(json.contains("\"failed\":1"));
}

#[test]
fn test_cache_skip_reason_display() {
    assert_eq!(format!("{}", CacheSkipReason::EmptyInputs), "empty inputs");
    assert_eq!(
        format!(
            "{}",
            CacheSkipReason::Disabled {
                reason: Some("hermetic".to_string())
            }
        ),
        "disabled: hermetic"
    );
}

#[test]
fn test_task_kind_default() {
    assert_eq!(TaskKind::default(), TaskKind::Task);
}

#[test]
fn test_ci_event_context_detected() {
    let event = CiEvent::ContextDetected {
        provider: "github".to_string(),
        event_type: "push".to_string(),
        ref_name: "main".to_string(),
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("ContextDetected"));
    assert!(json.contains("github"));
}

#[test]
fn test_ci_event_changed_files_found() {
    let event = CiEvent::ChangedFilesFound { count: 10 };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("ChangedFilesFound"));
    assert!(json.contains("10"));
}

#[test]
fn test_ci_event_projects_discovered() {
    let event = CiEvent::ProjectsDiscovered { count: 3 };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("ProjectsDiscovered"));
}

#[test]
fn test_ci_event_project_skipped() {
    let event = CiEvent::ProjectSkipped {
        path: "/project".to_string(),
        reason: "no changes".to_string(),
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("ProjectSkipped"));
    assert!(json.contains("no changes"));
}

#[test]
fn test_ci_event_task_executing() {
    let event = CiEvent::TaskExecuting {
        project: "/app".to_string(),
        task: "build".to_string(),
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("TaskExecuting"));
}

#[test]
fn test_ci_event_task_result() {
    let event = CiEvent::TaskResult {
        project: "/app".to_string(),
        task: "build".to_string(),
        success: false,
        error: Some("build failed".to_string()),
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("TaskResult"));
    assert!(json.contains("build failed"));
}

#[test]
fn test_ci_event_report_generated() {
    let event = CiEvent::ReportGenerated {
        path: "/reports/ci.json".to_string(),
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("ReportGenerated"));
}

#[test]
fn test_command_event_started() {
    let event = CommandEvent::Started {
        command: "sync".to_string(),
        args: vec!["--force".to_string()],
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("Started"));
    assert!(json.contains("--force"));
}

#[test]
fn test_command_event_progress() {
    let event = CommandEvent::Progress {
        command: "sync".to_string(),
        progress: 0.5,
        message: "halfway there".to_string(),
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("Progress"));
    assert!(json.contains("0.5"));
}

#[test]
fn test_command_event_completed() {
    let event = CommandEvent::Completed {
        command: "sync".to_string(),
        success: true,
        duration_ms: 500,
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("Completed"));
}

#[test]
fn test_interactive_event_prompt_requested() {
    let event = InteractiveEvent::PromptRequested {
        prompt_id: "p1".to_string(),
        message: "Choose an option".to_string(),
        options: vec!["a".to_string(), "b".to_string()],
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("PromptRequested"));
    assert!(json.contains("Choose an option"));
}

#[test]
fn test_interactive_event_prompt_resolved() {
    let event = InteractiveEvent::PromptResolved {
        prompt_id: "p1".to_string(),
        response: "a".to_string(),
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("PromptResolved"));
}

#[test]
fn test_interactive_event_wait_progress() {
    let event = InteractiveEvent::WaitProgress {
        target: "lock".to_string(),
        elapsed_secs: 30,
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("WaitProgress"));
    assert!(json.contains("30"));
}

#[test]
fn test_system_event_supervisor_log() {
    let event = SystemEvent::SupervisorLog {
        tag: "coordinator".to_string(),
        message: "started".to_string(),
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("SupervisorLog"));
}

#[test]
fn test_system_event_shutdown() {
    let event = SystemEvent::Shutdown;
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("Shutdown"));
}

#[test]
fn test_output_event_stdout() {
    let event = OutputEvent::Stdout {
        content: "hello".to_string(),
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("Stdout"));
    assert!(json.contains("hello"));
}

#[test]
fn test_output_event_stderr() {
    let event = OutputEvent::Stderr {
        content: "error".to_string(),
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("Stderr"));
}

#[test]
fn test_stream_enum() {
    assert_eq!(Stream::Stdout, Stream::Stdout);
    assert_ne!(Stream::Stdout, Stream::Stderr);

    let stdout_json = serde_json::to_string(&Stream::Stdout).unwrap();
    let stderr_json = serde_json::to_string(&Stream::Stderr).unwrap();

    assert!(stdout_json.contains("Stdout"));
    assert!(stderr_json.contains("Stderr"));
}

#[test]
fn test_event_category_all_variants() {
    let categories = vec![
        EventCategory::Task(TaskEvent::CacheMiss {
            name: "test".to_string(),
            parent_group: None,
        }),
        EventCategory::Service(ServiceEvent::Pending {
            name: "db".to_string(),
        }),
        EventCategory::Ci(CiEvent::ProjectsDiscovered { count: 1 }),
        EventCategory::Command(CommandEvent::Started {
            command: "sync".to_string(),
            args: vec![],
        }),
        EventCategory::Interactive(InteractiveEvent::WaitProgress {
            target: "lock".to_string(),
            elapsed_secs: 0,
        }),
        EventCategory::System(SystemEvent::Shutdown),
        EventCategory::Output(OutputEvent::Stdout {
            content: "out".to_string(),
        }),
    ];

    for cat in categories {
        let json = serde_json::to_string(&cat).unwrap();
        let parsed: EventCategory = serde_json::from_str(&json).unwrap();
        // Verify round-trip works
        let json2 = serde_json::to_string(&parsed).unwrap();
        assert_eq!(json, json2);
    }
}

#[test]
fn test_cuenv_event_clone() {
    let event = CuenvEvent::new(
        Uuid::new_v4(),
        EventSource::new("cuenv::test"),
        EventCategory::System(SystemEvent::Shutdown),
    );
    let cloned = event.clone();
    assert_eq!(event.id, cloned.id);
    assert_eq!(event.correlation_id, cloned.correlation_id);
}

#[test]
fn test_cuenv_event_debug() {
    let event = CuenvEvent::new(
        Uuid::new_v4(),
        EventSource::new("cuenv::test"),
        EventCategory::System(SystemEvent::Shutdown),
    );
    let debug_str = format!("{event:?}");
    assert!(debug_str.contains("CuenvEvent"));
}

#[test]
fn test_service_event_pending() {
    let event = ServiceEvent::Pending {
        name: "db".to_string(),
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("Pending"));
    assert!(json.contains("db"));
}

#[test]
fn test_service_event_starting() {
    let event = ServiceEvent::Starting {
        name: "db".to_string(),
        command: "postgres -D /data".to_string(),
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("Starting"));
    assert!(json.contains("postgres"));
}

#[test]
fn test_service_event_ready() {
    let event = ServiceEvent::Ready {
        name: "db".to_string(),
        after_ms: 1200,
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("Ready"));
    assert!(json.contains("1200"));
}

#[test]
fn test_service_event_restarting() {
    let event = ServiceEvent::Restarting {
        name: "api".to_string(),
        reason: RestartReason::WatchTriggered,
        attempt: 2,
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("Restarting"));
    assert!(json.contains("WatchTriggered"));
}

#[test]
fn test_service_event_stopped() {
    let event = ServiceEvent::Stopped {
        name: "web".to_string(),
        exit_code: Some(0),
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("Stopped"));
}

#[test]
fn test_service_event_failed() {
    let event = ServiceEvent::Failed {
        name: "api".to_string(),
        error: "readiness timeout".to_string(),
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("Failed"));
    assert!(json.contains("readiness timeout"));
}

#[test]
fn test_service_event_watch() {
    let event = ServiceEvent::Watch {
        name: "api".to_string(),
        changed: vec!["src/main.rs".to_string()],
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("Watch"));
    assert!(json.contains("src/main.rs"));
}

#[test]
fn test_service_event_output() {
    let event = ServiceEvent::Output {
        name: "db".to_string(),
        stream: Stream::Stdout,
        line: "ready to accept connections".to_string(),
    };
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("Output"));
    assert!(json.contains("ready to accept connections"));
}

#[test]
fn test_restart_reason_serialization() {
    let reasons = vec![
        RestartReason::Crashed,
        RestartReason::WatchTriggered,
        RestartReason::Manual,
    ];
    for reason in reasons {
        let json = serde_json::to_string(&reason).unwrap();
        let parsed: RestartReason = serde_json::from_str(&json).unwrap();
        let json2 = serde_json::to_string(&parsed).unwrap();
        assert_eq!(json, json2);
    }
}
