use super::*;
use cuenv_core::environment::{Env, EnvValue, EnvValueSimple, EnvVarWithPolicies};
use cuenv_core::manifest::Project;
use cuenv_core::secrets::Secret;

use std::collections::HashMap;
use std::path::Path;

#[test]
fn test_escape_shell_value() {
    // Test basic string
    assert_eq!(escape_shell_value("simple"), "simple");

    // Test double quotes
    assert_eq!(escape_shell_value("hello \"world\""), "hello \\\"world\\\"");

    // Test backslashes
    assert_eq!(escape_shell_value("path\\to\\file"), "path\\\\to\\\\file");

    // Test dollar signs
    assert_eq!(escape_shell_value("$HOME"), "\\$HOME");
    assert_eq!(escape_shell_value("test $var"), "test \\$var");

    // Test backticks
    assert_eq!(escape_shell_value("`command`"), "\\`command\\`");

    // Test multiple special characters
    assert_eq!(
        escape_shell_value("$HOME/path\\with\"quotes`and`backticks"),
        "\\$HOME/path\\\\with\\\"quotes\\`and\\`backticks"
    );

    // Test empty string
    assert_eq!(escape_shell_value(""), "");

    // Test string with newlines (not escaped)
    assert_eq!(escape_shell_value("line1\nline2"), "line1\nline2");

    // Test string with tabs (not escaped)
    assert_eq!(escape_shell_value("col1\tcol2"), "col1\tcol2");
}

#[test]
fn test_format_no_op_clears_state() {
    let bash = format_no_op(Shell::Bash);
    assert!(bash.contains("unset"));
    assert!(bash.contains("CUENV_PENDING_APPROVAL_DIR"));
    assert!(bash.contains("CUENV_LOADED_DIR"));
    assert!(bash.trim().ends_with(':'));

    let fish = format_no_op(Shell::Fish);
    assert!(fish.contains("set -e CUENV_PENDING_APPROVAL_DIR"));
    assert!(fish.contains("set -e CUENV_LOADED_DIR"));
    assert!(fish.trim().ends_with("true"));

    let zsh = format_no_op(Shell::Zsh);
    assert!(zsh.contains("unset"));
    assert!(zsh.contains("CUENV_PENDING_APPROVAL_DIR"));
    assert!(zsh.contains("CUENV_LOADED_DIR"));

    let pwsh = format_no_op(Shell::PowerShell);
    assert!(pwsh.contains("Remove-Item Env:CUENV_PENDING_APPROVAL_DIR"));
    assert!(pwsh.contains("Remove-Item Env:CUENV_LOADED_DIR"));
}

#[test]
fn test_format_not_allowed_emits_notice_and_clears_loaded() {
    let dir = Path::new("/tmp/project");
    let bash_notice = format_not_allowed(dir, Shell::Bash, 1);
    assert!(bash_notice.contains("cuenv detected env.cue"));
    assert!(bash_notice.contains("cuenv allow'")); // Simplified command
    assert!(bash_notice.contains("contains 1 hook"));
    assert!(bash_notice.contains("export CUENV_PENDING_APPROVAL_DIR="));
    assert!(bash_notice.contains("unset CUENV_LOADED_DIR"));

    let fish_notice = format_not_allowed(dir, Shell::Fish, 2);
    assert!(fish_notice.contains("set -x CUENV_PENDING_APPROVAL_DIR"));
    assert!(fish_notice.contains("cuenv detected env.cue"));
    assert!(fish_notice.contains("contains 2 hooks"));
    assert!(fish_notice.contains("set -e CUENV_LOADED_DIR"));
}

#[test]
fn test_format_env_diff_exports_and_loaded_message() {
    let dir = Path::new("/tmp/project");
    let mut env = HashMap::new();
    env.insert("FOO".to_string(), "bar baz".to_string());
    env.insert("NUM".to_string(), "42".to_string());

    let bash = format_env_diff(dir, env.clone(), Shell::Bash);
    assert!(bash.contains("echo \"Project environment loaded\" >&2"));
    assert!(bash.contains("export CUENV_LOADED_DIR=\"/tmp/project\""));
    assert!(bash.contains("export FOO=\"bar baz\""));
    assert!(bash.contains("export NUM=\"42\""));

    let zsh = format_env_diff(dir, env.clone(), Shell::Zsh);
    assert!(zsh.contains("echo \"Project environment loaded\" >&2"));
    assert!(zsh.contains("export FOO=\"bar baz\""));

    let fish = format_env_diff(dir, env.clone(), Shell::Fish);
    assert!(fish.contains("echo \"Project environment loaded\" >&2"));
    assert!(fish.contains("set -x CUENV_LOADED_DIR \"/tmp/project\""));
    assert!(fish.contains("set -x FOO \"bar baz\""));

    let pwsh = format_env_diff(dir, env, Shell::PowerShell);
    assert!(pwsh.contains("Write-Host 'Project environment loaded'"));
    assert!(pwsh.contains("$env:CUENV_LOADED_DIR = '/tmp/project'"));
    assert!(pwsh.contains("$env:FOO = \"bar baz\""));
}

#[test]
fn test_format_env_diff_with_unset() {
    let dir = Path::new("/tmp/project");
    let current = HashMap::from([
        ("A".to_string(), "1".to_string()),
        ("B".to_string(), "2".to_string()),
    ]);
    let previous = HashMap::from([
        ("A".to_string(), "old".to_string()),
        ("REMOVED".to_string(), "x".to_string()),
    ]);

    let out_bash = format_env_diff_with_unset(dir, current.clone(), Some(&previous), Shell::Bash);
    assert!(out_bash.lines().any(|l| l == "unset REMOVED"));
    assert!(out_bash.contains("export A=\"1\""));
    assert!(out_bash.contains("echo \"Project environment loaded\""));

    let out_fish = format_env_diff_with_unset(dir, current.clone(), Some(&previous), Shell::Fish);
    assert!(out_fish.lines().any(|l| l == "set -e REMOVED"));

    let out_pwsh = format_env_diff_with_unset(dir, current, Some(&previous), Shell::PowerShell);
    assert!(out_pwsh.lines().any(|l| l == "Remove-Item Env:REMOVED"));
}

#[test]
fn test_extract_static_env_vars_skips_secrets() {
    // Build Project with one normal var and one secret
    let mut base = HashMap::new();
    base.insert("PLAIN".to_string(), EnvValue::String("value".to_string()));
    let secret = Secret::new("cmd".to_string(), vec!["arg".to_string()]);
    base.insert("SECRET".to_string(), EnvValue::Secret(secret));

    let env_cfg = Env {
        base,
        environment: None,
    };
    let cfg = Project {
        config: None,
        env: Some(env_cfg),
        hooks: None,
        ci: None,
        tasks: HashMap::new(),
        name: "test".to_string(),
        codegen: None,
        runtime: None,
        formatters: None,
        services: HashMap::new(),
        images: HashMap::new(),
        vcs: HashMap::new(),
    };

    let vars = extract_static_env_vars(&cfg);
    assert!(vars.get("PLAIN") == Some(&"value".to_string()));
    assert!(!vars.contains_key("SECRET"));
}

#[test]
fn test_collect_all_env_vars_override() {
    let mut base = HashMap::new();
    base.insert("OVERRIDE".to_string(), EnvValue::String("base".to_string()));
    base.insert(
        "BASE_ONLY".to_string(),
        EnvValue::WithPolicies(EnvVarWithPolicies {
            value: EnvValueSimple::String("plain".to_string()),
            policies: None,
        }),
    );

    let cfg = Project {
        config: None,
        env: Some(Env {
            base,
            environment: None,
        }),
        hooks: None,
        ci: None,
        tasks: HashMap::new(),
        name: "test".to_string(),
        codegen: None,
        runtime: None,
        formatters: None,
        services: HashMap::new(),
        images: HashMap::new(),
        vcs: HashMap::new(),
    };

    let hook_env = HashMap::from([
        ("OVERRIDE".to_string(), "hook".to_string()),
        ("HOOK_ONLY".to_string(), "x".to_string()),
    ]);

    let merged = collect_all_env_vars(&cfg, &hook_env);
    assert_eq!(merged.get("OVERRIDE"), Some(&"hook".to_string()));
    assert_eq!(merged.get("BASE_ONLY"), Some(&"plain".to_string()));
    assert_eq!(merged.get("HOOK_ONLY"), Some(&"x".to_string()));
}
