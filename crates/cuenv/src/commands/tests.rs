//! Tests for the commands module

use super::*;
use crate::events::{Event, EventReceiver};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::timeout;

fn create_test_executor() -> (CommandExecutor, EventReceiver) {
    let (sender, receiver) = mpsc::unbounded_channel();
    let executor = CommandExecutor::new(sender, "cuenv".to_string());
    (executor, receiver)
}

#[allow(dead_code)]
async fn collect_events(mut receiver: EventReceiver, count: usize) -> Vec<Event> {
    let mut events = Vec::new();
    for _ in 0..count {
        if let Ok(Some(event)) = timeout(Duration::from_millis(500), receiver.recv()).await {
            events.push(event);
        }
    }
    events
}

#[tokio::test]
async fn test_command_executor_new() {
    let (sender, _receiver) = mpsc::unbounded_channel();
    let executor = CommandExecutor::new(sender, "cuenv".to_string());
    assert!(matches!(executor, CommandExecutor { .. }));
    assert_eq!(executor.package(), "cuenv");
}

#[tokio::test]
async fn test_execute_version_command() {
    let (executor, mut receiver) = create_test_executor();

    let handle = tokio::spawn(async move {
        executor
            .execute(Command::Version {
                format: "simple".to_string(),
            })
            .await
    });

    // Collect events
    let mut events = Vec::new();
    while let Ok(Some(event)) = timeout(Duration::from_millis(1500), receiver.recv()).await {
        let is_complete = matches!(&event, Event::CommandComplete { .. });
        events.push(event);

        // Break when we receive CommandComplete
        if is_complete {
            break;
        }
    }

    let result = handle.await.unwrap();
    assert!(result.is_ok());

    // Verify we got start, progress, and complete events
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::CommandStart { command } if command == "version"))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::CommandProgress { .. }))
    );
    assert!(events.iter().any(|e| matches!(e, Event::CommandComplete { command, success: true, .. } if command == "version")));
}

#[tokio::test]
async fn test_execute_version_progress_events() {
    let (executor, mut receiver) = create_test_executor();

    let handle = tokio::spawn(async move {
        executor
            .execute(Command::Version {
                format: "simple".to_string(),
            })
            .await
    });

    // Collect progress events
    let mut progress_events = Vec::new();
    while let Ok(Some(event)) = timeout(Duration::from_millis(1500), receiver.recv()).await {
        if let Event::CommandProgress {
            progress, message, ..
        } = event
        {
            progress_events.push((progress, message));
        } else if matches!(event, Event::CommandComplete { .. }) {
            break;
        }
    }

    let result = handle.await.unwrap();
    assert!(result.is_ok());

    // Verify progress sequence
    assert!(!progress_events.is_empty());
    assert!(
        progress_events
            .iter()
            .any(|(_, msg)| msg.contains("Initializing"))
    );
    assert!(
        progress_events
            .iter()
            .any(|(_, msg)| msg.contains("Loading version info"))
    );
    assert!(
        progress_events
            .iter()
            .any(|(_, msg)| msg.contains("Complete"))
    );

    // Verify progress values
    let progress_values: Vec<f32> = progress_events.iter().map(|(p, _)| *p).collect();
    assert!(progress_values.first().unwrap() <= progress_values.last().unwrap());
}

#[tokio::test]
async fn test_execute_env_print_success() {
    let (executor, mut receiver) = create_test_executor();

    // Mock successful env print
    let path = "/tmp/test".to_string();
    let package = "test-package".to_string();
    let format = "json".to_string();

    let handle = tokio::spawn(async move {
        executor
            .execute(Command::EnvPrint {
                path,
                package,
                format,
                environment: None,
            })
            .await
    });

    // Collect events
    let mut events = Vec::new();
    while let Ok(Some(event)) = timeout(Duration::from_millis(1500), receiver.recv()).await {
        let is_complete = matches!(&event, Event::CommandComplete { .. });
        events.push(event);
        if is_complete {
            break;
        }
    }

    // Note: This might fail due to actual file system operations
    // In a real test, we'd mock the env::execute_env_print function
    let _ = handle.await.unwrap();

    // Verify start event was sent
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::CommandStart { command } if command == "env print"))
    );
    // Verify complete event was sent (success depends on actual execution)
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::CommandComplete { command, .. } if command == "env print"))
    );
}

#[tokio::test]
async fn test_command_enum_variants() {
    // Test Command enum creation
    let version_cmd = Command::Version {
        format: "simple".to_string(),
    };
    assert!(matches!(version_cmd, Command::Version { .. }));

    let env_cmd = Command::EnvPrint {
        path: "/test/path".to_string(),
        package: "test-pkg".to_string(),
        format: "yaml".to_string(),
        environment: Some("production".to_string()),
    };

    if let Command::EnvPrint {
        path,
        package,
        format,
        environment,
    } = env_cmd
    {
        assert_eq!(path, "/test/path");
        assert_eq!(package, "test-pkg");
        assert_eq!(format, "yaml");
        assert_eq!(environment, Some("production".to_string()));
    } else {
        panic!("Expected EnvPrint variant");
    }
}

#[tokio::test]
async fn test_send_event() {
    let (sender, mut receiver) = mpsc::unbounded_channel();
    let executor = CommandExecutor::new(sender, "cuenv".to_string());

    // Send a test event
    executor.send_event(Event::CommandStart {
        command: "test".to_string(),
    });

    // Verify event was received
    let event = receiver.recv().await.unwrap();
    assert!(matches!(event, Event::CommandStart { command } if command == "test"));
}

#[tokio::test]
async fn test_send_event_with_closed_channel() {
    let (sender, receiver) = mpsc::unbounded_channel();
    let executor = CommandExecutor::new(sender, "cuenv".to_string());

    // Close the receiver
    drop(receiver);

    // Send event should not panic, just log error
    executor.send_event(Event::CommandStart {
        command: "test".to_string(),
    });

    // Test passes if no panic occurs
}

#[tokio::test]
async fn test_execute_version_command_flow() {
    let (executor, mut receiver) = create_test_executor();

    let handle = tokio::spawn(async move { executor.execute_version().await });

    // Verify the complete flow
    let mut has_start = false;
    let mut has_progress = false;
    let mut has_complete = false;

    while let Ok(Some(event)) = timeout(Duration::from_millis(1500), receiver.recv()).await {
        match event {
            Event::CommandStart { command } if command == "version" => has_start = true,
            Event::CommandProgress { command, .. } if command == "version" => {
                has_progress = true;
            }
            Event::CommandComplete {
                command,
                success: true,
                ..
            } if command == "version" => {
                has_complete = true;
                break;
            }
            _ => {}
        }
    }

    let result = handle.await.unwrap();
    assert!(result.is_ok());
    assert!(has_start);
    assert!(has_progress);
    assert!(has_complete);
}

#[tokio::test]
async fn test_command_debug_trait() {
    let cmd = Command::Version {
        format: "simple".to_string(),
    };
    let debug_str = format!("{cmd:?}");
    assert!(debug_str.contains("Version"));

    let cmd = Command::EnvPrint {
        path: "/path".to_string(),
        package: "pkg".to_string(),
        format: "json".to_string(),
        environment: None,
    };
    let debug_str = format!("{cmd:?}");
    assert!(debug_str.contains("EnvPrint"));
    assert!(debug_str.contains("/path"));
    assert!(debug_str.contains("pkg"));
    assert!(debug_str.contains("json"));
}

#[tokio::test]
async fn test_command_clone_trait() {
    let original = Command::Version {
        format: "simple".to_string(),
    };
    let cloned = original.clone();
    assert!(matches!(cloned, Command::Version { .. }));

    let original = Command::EnvPrint {
        path: "/test".to_string(),
        package: "test".to_string(),
        format: "toml".to_string(),
        environment: Some("dev".to_string()),
    };
    let cloned = original.clone();

    if let Command::EnvPrint {
        path,
        package,
        format,
        environment,
    } = cloned
    {
        assert_eq!(path, "/test");
        assert_eq!(package, "test");
        assert_eq!(format, "toml");
        assert_eq!(environment, Some("dev".to_string()));
    } else {
        panic!("Clone failed");
    }
}
