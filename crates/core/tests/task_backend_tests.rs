//! Tests for task backend system

use cuenv_core::config::BackendConfig;
use cuenv_core::environment::Environment;
use cuenv_core::tasks::{
    HostBackend, Task, TaskBackend, create_backend, create_backend_with_factory,
};
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;

#[tokio::test]
async fn test_host_backend_execute_simple_command() {
    let backend = HostBackend::new();
    let task = Task {
        command: "echo".to_string(),
        args: vec!["hello".to_string()],
        ..Default::default()
    };
    let env = Environment::default();
    let temp_dir = TempDir::new().unwrap();

    let result = backend
        .execute("test", &task, &env, temp_dir.path(), true)
        .await
        .unwrap();

    assert!(result.success);
    assert!(result.stdout.contains("hello"));
    assert_eq!(result.name, "test");
}

#[tokio::test]
async fn test_host_backend_execute_failing_command() {
    let backend = HostBackend::new();
    let task = Task {
        command: "false".to_string(),
        args: vec![],
        ..Default::default()
    };
    let env = Environment::default();
    let temp_dir = TempDir::new().unwrap();

    let result = backend
        .execute("test", &task, &env, temp_dir.path(), true)
        .await
        .unwrap();

    assert!(!result.success);
    assert!(result.exit_code.is_some());
    assert_ne!(result.exit_code.unwrap(), 0);
}

#[tokio::test]
async fn test_host_backend_execute_with_args() {
    let backend = HostBackend::new();
    let task = Task {
        command: "echo".to_string(),
        args: vec!["arg1".to_string(), "arg2".to_string()],
        ..Default::default()
    };
    let env = Environment::default();
    let temp_dir = TempDir::new().unwrap();

    let result = backend
        .execute("test", &task, &env, temp_dir.path(), true)
        .await
        .unwrap();

    assert!(result.success);
    assert!(result.stdout.contains("arg1"));
    assert!(result.stdout.contains("arg2"));
}

#[tokio::test]
async fn test_host_backend_execute_with_env_vars() {
    let backend = HostBackend::new();
    
    // Use sh -c to access environment variables (more portable than echo)
    let task = Task {
        command: "sh".to_string(),
        args: vec!["-c".to_string(), "echo $TEST_VAR".to_string()],
        ..Default::default()
    };
    
    let mut env = Environment::default();
    env.vars.insert("TEST_VAR".to_string(), "test_value".to_string());
    
    let temp_dir = TempDir::new().unwrap();

    let result = backend
        .execute("test", &task, &env, temp_dir.path(), true)
        .await
        .unwrap();

    assert!(result.success);
    assert!(result.stdout.contains("test_value"));
}

#[tokio::test]
async fn test_host_backend_execute_with_working_directory() {
    let backend = HostBackend::new();
    let temp_dir = TempDir::new().unwrap();
    
    // Create a test file in the working directory
    std::fs::write(temp_dir.path().join("test.txt"), "content").unwrap();
    
    let task = Task {
        command: "ls".to_string(),
        args: vec![],
        ..Default::default()
    };
    let env = Environment::default();

    let result = backend
        .execute("test", &task, &env, temp_dir.path(), true)
        .await
        .unwrap();

    assert!(result.success);
    assert!(result.stdout.contains("test.txt"));
}

#[tokio::test]
async fn test_host_backend_execute_nonexistent_command() {
    let backend = HostBackend::new();
    let task = Task {
        command: "nonexistent_command_12345".to_string(),
        args: vec![],
        ..Default::default()
    };
    let env = Environment::default();
    let temp_dir = TempDir::new().unwrap();

    let result = backend
        .execute("test", &task, &env, temp_dir.path(), true)
        .await;

    assert!(result.is_err());
}

#[tokio::test]
async fn test_host_backend_name() {
    let backend = HostBackend::new();
    assert_eq!(backend.name(), "host");
}

#[tokio::test]
async fn test_host_backend_default() {
    let backend = HostBackend::default();
    assert_eq!(backend.name(), "host");
}

#[tokio::test]
async fn test_host_backend_capture_output_false() {
    let backend = HostBackend::new();
    let task = Task {
        command: "echo".to_string(),
        args: vec!["test".to_string()],
        ..Default::default()
    };
    let env = Environment::default();
    let temp_dir = TempDir::new().unwrap();

    let result = backend
        .execute("test", &task, &env, temp_dir.path(), false)
        .await
        .unwrap();

    assert!(result.success);
    // When not capturing, stdout/stderr are empty (output went to terminal)
    assert_eq!(result.stdout, "");
    assert_eq!(result.stderr, "");
}

#[test]
fn test_create_backend_default_host() {
    let backend = create_backend(None, PathBuf::from("/tmp"), None);
    assert_eq!(backend.name(), "host");
}

#[test]
fn test_create_backend_with_host_config() {
    let config = BackendConfig {
        backend_type: "host".to_string(),
        ..Default::default()
    };
    let backend = create_backend(Some(&config), PathBuf::from("/tmp"), None);
    assert_eq!(backend.name(), "host");
}

#[test]
fn test_create_backend_with_cli_override() {
    let config = BackendConfig {
        backend_type: "dagger".to_string(),
        ..Default::default()
    };
    // CLI override should take precedence
    let backend = create_backend(Some(&config), PathBuf::from("/tmp"), Some("host"));
    assert_eq!(backend.name(), "host");
}

#[test]
fn test_create_backend_dagger_without_factory() {
    // Requesting dagger without a factory should fall back to host
    let backend = create_backend(None, PathBuf::from("/tmp"), Some("dagger"));
    assert_eq!(backend.name(), "host");
}

#[test]
fn test_create_backend_with_factory() {
    // Mock factory that returns a host backend for testing
    fn mock_factory(_config: Option<&BackendConfig>, _root: PathBuf) -> Arc<dyn TaskBackend> {
        Arc::new(HostBackend::new())
    }

    let backend = create_backend_with_factory(
        None,
        PathBuf::from("/tmp"),
        Some("dagger"),
        Some(mock_factory),
    );

    // Factory should be called for dagger backend
    assert_eq!(backend.name(), "host"); // Our mock returns host
}

#[test]
fn test_create_backend_unknown_backend_type() {
    // Unknown backend type should default to host
    let backend = create_backend(None, PathBuf::from("/tmp"), Some("unknown"));
    assert_eq!(backend.name(), "host");
}

#[tokio::test]
async fn test_host_backend_with_stderr() {
    let backend = HostBackend::new();
    let task = Task {
        command: "sh".to_string(),
        args: vec!["-c".to_string(), "echo error >&2".to_string()],
        ..Default::default()
    };
    let env = Environment::default();
    let temp_dir = TempDir::new().unwrap();

    let result = backend
        .execute("test", &task, &env, temp_dir.path(), true)
        .await
        .unwrap();

    assert!(result.success);
    assert!(result.stderr.contains("error"));
}

#[tokio::test]
async fn test_host_backend_exit_code_propagation() {
    let backend = HostBackend::new();
    let task = Task {
        command: "sh".to_string(),
        args: vec!["-c".to_string(), "exit 42".to_string()],
        ..Default::default()
    };
    let env = Environment::default();
    let temp_dir = TempDir::new().unwrap();

    let result = backend
        .execute("test", &task, &env, temp_dir.path(), true)
        .await
        .unwrap();

    assert!(!result.success);
    assert_eq!(result.exit_code, Some(42));
}

#[tokio::test]
async fn test_host_backend_task_result_name() {
    let backend = HostBackend::new();
    let task = Task {
        command: "echo".to_string(),
        args: vec!["test".to_string()],
        ..Default::default()
    };
    let env = Environment::default();
    let temp_dir = TempDir::new().unwrap();

    let result = backend
        .execute("my-task-name", &task, &env, temp_dir.path(), true)
        .await
        .unwrap();

    assert_eq!(result.name, "my-task-name");
}
