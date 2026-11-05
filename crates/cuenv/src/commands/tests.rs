//! Tests for the commands module

use super::*;

#[tokio::test]
async fn test_command_executor_new() {
    let executor = CommandExecutor::new("cuenv".to_string());
    assert!(matches!(executor, CommandExecutor { .. }));
    assert_eq!(executor.package(), "cuenv");
}

#[tokio::test]
async fn test_execute_env_print_fails_on_invalid_path() {
    let executor = CommandExecutor::new("cuenv".to_string());

    // Use a path that should not resolve to a valid cuenv project.
    let result = executor
        .execute(Command::EnvPrint {
            path: "/definitely-not-a-cuenv-project".to_string(),
            package: "test-package".to_string(),
            format: "json".to_string(),
            environment: None,
        })
        .await;

    assert!(result.is_err());
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
