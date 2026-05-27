use super::*;
use std::fs;
use tempfile::TempDir;

#[test]
fn test_extract_hooks_from_config() {
    use cuenv_core::manifest::Project;
    use cuenv_hooks::{Hook, Hooks};
    use std::collections::HashMap;

    let mut on_enter = HashMap::new();
    on_enter.insert(
        "npm".to_string(),
        Hook {
            order: 100,
            propagate: false,
            command: "npm".to_string(),
            args: vec!["install".to_string()],
            dir: None,
            inputs: vec![],
            source: None,
        },
    );
    on_enter.insert(
        "docker".to_string(),
        Hook {
            order: 100,
            propagate: false,
            command: "docker-compose".to_string(),
            args: vec!["up".to_string(), "-d".to_string()],
            dir: None,
            inputs: vec![],
            source: None,
        },
    );

    let config = Project {
        config: None,
        env: None,
        hooks: Some(Hooks {
            on_enter: Some(on_enter),
            on_exit: None,
            pre_push: None,
        }),
        ci: None,
        tasks: std::collections::HashMap::new(),
        name: "test".to_string(),
        codegen: None,
        formatters: None,
        runtime: None,
        services: std::collections::HashMap::new(),
        images: std::collections::HashMap::new(),
        vcs: std::collections::HashMap::new(),
    };

    let hooks = extract_hooks_from_config(&config);
    assert_eq!(hooks.len(), 2);
    // Sorted alphabetically by name when order is equal
    assert_eq!(hooks[0].command, "docker-compose");
    assert_eq!(hooks[1].command, "npm");
}

#[test]
fn test_extract_hooks_single_hook() {
    use cuenv_core::manifest::Project;
    use cuenv_hooks::{Hook, Hooks};
    use std::collections::HashMap;

    let mut on_enter = HashMap::new();
    on_enter.insert(
        "echo".to_string(),
        Hook {
            order: 100,
            propagate: false,
            command: "echo".to_string(),
            args: vec!["hello".to_string()],
            dir: None,
            inputs: vec![],
            source: None,
        },
    );

    let config = Project {
        config: None,
        env: None,
        hooks: Some(Hooks {
            on_enter: Some(on_enter),
            on_exit: None,
            pre_push: None,
        }),
        ci: None,
        tasks: std::collections::HashMap::new(),
        name: "test".to_string(),
        codegen: None,
        formatters: None,
        runtime: None,
        services: std::collections::HashMap::new(),
        images: std::collections::HashMap::new(),
        vcs: std::collections::HashMap::new(),
    };

    let hooks = extract_hooks_from_config(&config);
    assert_eq!(hooks.len(), 1);
    assert_eq!(hooks[0].command, "echo");
    assert_eq!(hooks[0].args, vec!["hello"]);
}

#[test]
fn test_extract_hooks_empty_config() {
    use cuenv_core::manifest::Project;

    let config = Project {
        config: None,
        env: None,
        hooks: None,
        ci: None,
        tasks: std::collections::HashMap::new(),
        name: "test".to_string(),
        codegen: None,
        formatters: None,
        runtime: None,
        services: std::collections::HashMap::new(),
        images: std::collections::HashMap::new(),
        vcs: std::collections::HashMap::new(),
    };

    let hooks = extract_hooks_from_config(&config);
    assert_eq!(hooks.len(), 0);
}

#[test]
fn test_shell_integration_generation() {
    let fish_script = execute_shell_init(crate::cli::ShellType::Fish);
    assert!(fish_script.contains("function __cuenv_hook"));
    assert!(fish_script.contains("on-variable PWD"));

    let bash_script = execute_shell_init(crate::cli::ShellType::Bash);
    assert!(bash_script.contains("__cuenv_hook()"));
    assert!(bash_script.contains("PROMPT_COMMAND"));

    let zsh_script = execute_shell_init(crate::cli::ShellType::Zsh);
    assert!(zsh_script.contains("add-zsh-hook"));
    assert!(zsh_script.contains("precmd"));
}

#[tokio::test]
async fn test_execute_allow_no_directory() {
    let result = execute_allow("/nonexistent/directory", "cuenv", None, false, None).await;
    assert!(result.is_err());
    // The error type is Configuration error, which doesn't include the detailed message in Display
    // Just verify it's an error for a non-existent directory
    assert!(matches!(
        result.unwrap_err(),
        cuenv_core::Error::Configuration { .. }
    ));
}

#[tokio::test]
async fn test_execute_allow_no_env_cue() {
    let temp_dir = TempDir::new().unwrap();
    let result = execute_allow(
        temp_dir.path().to_str().unwrap(),
        "cuenv",
        None,
        false,
        None,
    )
    .await;
    assert!(result.is_err());
    // The error type is Configuration error for missing env.cue file
    assert!(matches!(
        result.unwrap_err(),
        cuenv_core::Error::Configuration { .. }
    ));
}

#[tokio::test]
async fn test_execute_env_load_no_file() {
    let temp_dir = TempDir::new().unwrap();
    let result = execute_env_load(temp_dir.path().to_str().unwrap(), "cuenv", None).await;
    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(output.contains("No env.cue file found"));
}

#[tokio::test]
async fn test_execute_env_status_no_file() {
    let temp_dir = TempDir::new().unwrap();
    let result = execute_env_status(
        temp_dir.path().to_str().unwrap(),
        "cuenv",
        false,
        30,
        StatusFormat::Text,
        None,
    )
    .await;
    assert!(result.is_ok());
    let output = result.unwrap();
    assert!(output.contains("No env.cue file found"));
}

#[tokio::test]
async fn test_execute_env_load_package_mismatch_message() {
    let temp_dir = TempDir::new().unwrap();
    fs::write(temp_dir.path().join("env.cue"), "package other\n\nenv: {}").unwrap();

    let output = execute_env_load(temp_dir.path().to_str().unwrap(), "cuenv", None)
        .await
        .unwrap();
    assert!(output.contains("uses package 'other'"));
}
