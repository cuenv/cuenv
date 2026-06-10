use super::*;

#[test]
fn test_environment_basics() {
    let mut env = Environment::new();
    assert!(env.is_empty());

    env.set("FOO".to_string(), "bar".to_string());
    assert_eq!(env.len(), 1);
    assert!(env.contains("FOO"));
    assert_eq!(env.get("FOO"), Some("bar"));
    assert!(!env.contains("BAR"));
}

#[test]
fn test_environment_from_map() {
    let mut vars = HashMap::new();
    vars.insert("KEY1".to_string(), "value1".to_string());
    vars.insert("KEY2".to_string(), "value2".to_string());

    let env = Environment::from_map(vars);
    assert_eq!(env.len(), 2);
    assert_eq!(env.get("KEY1"), Some("value1"));
    assert_eq!(env.get("KEY2"), Some("value2"));
}

#[test]
fn test_environment_to_vec() {
    let mut env = Environment::new();
    env.set("VAR1".to_string(), "val1".to_string());
    env.set("VAR2".to_string(), "val2".to_string());

    let vec = env.to_env_vec();
    assert_eq!(vec.len(), 2);
    assert!(vec.contains(&"VAR1=val1".to_string()));
    assert!(vec.contains(&"VAR2=val2".to_string()));
}

#[test]
fn test_environment_merge_with_system() {
    let mut env = Environment::new();
    env.set("PATH".to_string(), "/custom/path".to_string());
    env.set("CUSTOM_VAR".to_string(), "custom_value".to_string());

    let merged = env.merge_with_system();

    // Custom variables should be present
    assert_eq!(merged.get("PATH"), Some(&"/custom/path".to_string()));
    assert_eq!(merged.get("CUSTOM_VAR"), Some(&"custom_value".to_string()));

    // System variables should still be present (like HOME, USER, etc.)
    // We can't test specific values but we can check that merging happened
    assert!(merged.len() >= 2);
}

#[test]
fn test_hermetic_merge_omits_missing_system_temp_dirs() {
    let temp = tempfile::tempdir().expect("tempdir");
    let missing = temp.path().join("removed");
    let missing = missing.to_string_lossy().into_owned();

    temp_env::with_var("TMPDIR", Some(missing.as_str()), || {
        let env = Environment::new();
        let merged = env.merge_with_system_hermetic();

        assert!(!merged.contains_key("TMPDIR"));
    });
}

#[test]
fn test_hermetic_merge_keeps_project_temp_dir_override() {
    let system_temp = tempfile::tempdir().expect("system tempdir");
    let project_temp = tempfile::tempdir().expect("project tempdir");
    let system_temp = system_temp.path().to_string_lossy().into_owned();
    let project_temp = project_temp.path().to_string_lossy().into_owned();

    temp_env::with_var("TMPDIR", Some(system_temp.as_str()), || {
        let mut env = Environment::new();
        env.set("TMPDIR".to_string(), project_temp.clone());

        let merged = env.merge_with_system_hermetic();

        assert_eq!(merged.get("TMPDIR"), Some(&project_temp));
    });
}

#[test]
fn test_environment_iteration() {
    let mut env = Environment::new();
    env.set("A".to_string(), "1".to_string());
    env.set("B".to_string(), "2".to_string());

    let mut count = 0;
    for (key, value) in env.iter() {
        assert!(key == "A" || key == "B");
        assert!(value == "1" || value == "2");
        count += 1;
    }
    assert_eq!(count, 2);
}

#[test]
fn test_env_value_types() {
    let str_val = EnvValue::String("test".to_string());
    let int_val = EnvValue::Int(42);
    let bool_val = EnvValue::Bool(true);

    assert_eq!(str_val, EnvValue::String("test".to_string()));
    assert_eq!(int_val, EnvValue::Int(42));
    assert_eq!(bool_val, EnvValue::Bool(true));
}

#[test]
fn test_policy_task_access() {
    // Simple value - always accessible
    let simple_var = EnvValue::String("simple".to_string());
    assert!(simple_var.is_accessible_by_task("any_task"));

    // Variable with no policies - accessible
    let no_policy_var = EnvValue::WithPolicies(EnvVarWithPolicies {
        value: EnvValueSimple::String("value".to_string()),
        policies: None,
    });
    assert!(no_policy_var.is_accessible_by_task("any_task"));

    // Variable with empty policies - accessible
    let empty_policy_var = EnvValue::WithPolicies(EnvVarWithPolicies {
        value: EnvValueSimple::String("value".to_string()),
        policies: Some(vec![]),
    });
    assert!(empty_policy_var.is_accessible_by_task("any_task"));

    // Variable with task restrictions
    let restricted_var = EnvValue::WithPolicies(EnvVarWithPolicies {
        value: EnvValueSimple::String("secret".to_string()),
        policies: Some(vec![Policy {
            allow_tasks: Some(vec!["deploy".to_string(), "release".to_string()]),
            allow_exec: None,
        }]),
    });
    assert!(restricted_var.is_accessible_by_task("deploy"));
    assert!(restricted_var.is_accessible_by_task("release"));
    assert!(!restricted_var.is_accessible_by_task("test"));
    assert!(!restricted_var.is_accessible_by_task("build"));
}

#[test]
fn test_policy_exec_access() {
    // Simple value - always accessible
    let simple_var = EnvValue::String("simple".to_string());
    assert!(simple_var.is_accessible_by_exec("bash"));

    // Variable with exec restrictions
    let restricted_var = EnvValue::WithPolicies(EnvVarWithPolicies {
        value: EnvValueSimple::String("secret".to_string()),
        policies: Some(vec![Policy {
            allow_tasks: None,
            allow_exec: Some(vec!["kubectl".to_string(), "terraform".to_string()]),
        }]),
    });
    assert!(restricted_var.is_accessible_by_exec("kubectl"));
    assert!(restricted_var.is_accessible_by_exec("terraform"));
    assert!(!restricted_var.is_accessible_by_exec("bash"));
    assert!(!restricted_var.is_accessible_by_exec("sh"));
}

#[test]
fn test_multiple_policies() {
    // Variable with multiple policies - should allow if ANY policy allows
    let multi_policy_var = EnvValue::WithPolicies(EnvVarWithPolicies {
        value: EnvValueSimple::String("value".to_string()),
        policies: Some(vec![
            Policy {
                allow_tasks: Some(vec!["task1".to_string()]),
                allow_exec: None,
            },
            Policy {
                allow_tasks: Some(vec!["task2".to_string()]),
                allow_exec: Some(vec!["kubectl".to_string()]),
            },
        ]),
    });

    // Task access - either policy allows
    assert!(multi_policy_var.is_accessible_by_task("task1"));
    assert!(multi_policy_var.is_accessible_by_task("task2"));
    assert!(!multi_policy_var.is_accessible_by_task("task3"));

    // Exec access - only second policy has exec rules
    assert!(multi_policy_var.is_accessible_by_exec("kubectl"));
    assert!(!multi_policy_var.is_accessible_by_exec("bash"));
}

#[test]
fn test_to_string_value() {
    assert_eq!(
        EnvValue::String("test".to_string()).to_string_value(),
        "test"
    );
    assert_eq!(EnvValue::Int(42).to_string_value(), "42");
    assert_eq!(EnvValue::Bool(true).to_string_value(), "true");
    assert_eq!(EnvValue::Bool(false).to_string_value(), "false");

    let with_policies = EnvValue::WithPolicies(EnvVarWithPolicies {
        value: EnvValueSimple::String("policy_value".to_string()),
        policies: Some(vec![]),
    });
    assert_eq!(with_policies.to_string_value(), "policy_value");
}

#[test]
fn test_build_for_task() {
    let mut env_vars = HashMap::new();

    // Unrestricted variable
    env_vars.insert(
        "PUBLIC".to_string(),
        EnvValue::String("public_value".to_string()),
    );

    // Restricted variable
    env_vars.insert(
        "SECRET".to_string(),
        EnvValue::WithPolicies(EnvVarWithPolicies {
            value: EnvValueSimple::String("secret_value".to_string()),
            policies: Some(vec![Policy {
                allow_tasks: Some(vec!["deploy".to_string()]),
                allow_exec: None,
            }]),
        }),
    );

    // Build for deploy task - should get both
    let deploy_env = Environment::build_for_task("deploy", &env_vars);
    assert_eq!(deploy_env.len(), 2);
    assert_eq!(deploy_env.get("PUBLIC"), Some(&"public_value".to_string()));
    assert_eq!(deploy_env.get("SECRET"), Some(&"secret_value".to_string()));

    // Build for test task - should only get public
    let test_env = Environment::build_for_task("test", &env_vars);
    assert_eq!(test_env.len(), 1);
    assert_eq!(test_env.get("PUBLIC"), Some(&"public_value".to_string()));
    assert_eq!(test_env.get("SECRET"), None);
}

#[test]
fn test_build_for_exec() {
    let mut env_vars = HashMap::new();

    // Unrestricted variable
    env_vars.insert(
        "PUBLIC".to_string(),
        EnvValue::String("public_value".to_string()),
    );

    // Restricted variable
    env_vars.insert(
        "SECRET".to_string(),
        EnvValue::WithPolicies(EnvVarWithPolicies {
            value: EnvValueSimple::String("secret_value".to_string()),
            policies: Some(vec![Policy {
                allow_tasks: None,
                allow_exec: Some(vec!["kubectl".to_string()]),
            }]),
        }),
    );

    // Build for kubectl - should get both
    let kubectl_env = Environment::build_for_exec("kubectl", &env_vars);
    assert_eq!(kubectl_env.len(), 2);
    assert_eq!(kubectl_env.get("PUBLIC"), Some(&"public_value".to_string()));
    assert_eq!(kubectl_env.get("SECRET"), Some(&"secret_value".to_string()));

    // Build for bash - should only get public
    let bash_env = Environment::build_for_exec("bash", &env_vars);
    assert_eq!(bash_env.len(), 1);
    assert_eq!(bash_env.get("PUBLIC"), Some(&"public_value".to_string()));
    assert_eq!(bash_env.get("SECRET"), None);
}

#[test]
fn test_env_for_environment() {
    let mut base = HashMap::new();
    base.insert("BASE_VAR".to_string(), EnvValue::String("base".to_string()));
    base.insert(
        "OVERRIDE_ME".to_string(),
        EnvValue::String("original".to_string()),
    );

    let mut dev_env = HashMap::new();
    dev_env.insert(
        "OVERRIDE_ME".to_string(),
        EnvValue::String("dev".to_string()),
    );
    dev_env.insert(
        "DEV_VAR".to_string(),
        EnvValue::String("development".to_string()),
    );

    let mut environments = HashMap::new();
    environments.insert("development".to_string(), dev_env);

    let env = Env {
        base,
        environment: Some(environments),
    };

    let dev_vars = env.for_environment("development");
    assert_eq!(
        dev_vars.get("BASE_VAR"),
        Some(&EnvValue::String("base".to_string()))
    );
    assert_eq!(
        dev_vars.get("OVERRIDE_ME"),
        Some(&EnvValue::String("dev".to_string()))
    );
    assert_eq!(
        dev_vars.get("DEV_VAR"),
        Some(&EnvValue::String("development".to_string()))
    );
}

#[test]
fn test_env_deserialize_with_environment_overrides() {
    let json = r#"{
            "API_URL": "https://api.example.com",
            "environment": {
                "production": {
                    "API_URL": "https://api.prod.example.com",
                    "AUTH_SECRET": {"resolver": "exec", "command": "echo", "args": ["token"]}
                }
            }
        }"#;

    let env: Env = serde_json::from_str(json).expect("valid env payload");

    assert!(env.base.contains_key("API_URL"));
    assert!(!env.base.contains_key("environment"));

    let environments = env
        .environment
        .expect("environment overrides should deserialize");
    let production = environments
        .get("production")
        .expect("production overrides should exist");
    assert!(production.contains_key("AUTH_SECRET"));
}

#[tokio::test]
async fn test_resolve_plain_string() {
    let env_val = EnvValue::String("plain_value".to_string());
    let resolved = env_val.resolve().await.unwrap();
    assert_eq!(resolved, "plain_value");
}

#[tokio::test]
async fn test_resolve_int() {
    let env_val = EnvValue::Int(42);
    let resolved = env_val.resolve().await.unwrap();
    assert_eq!(resolved, "42");
}

#[tokio::test]
async fn test_resolve_bool() {
    let env_val = EnvValue::Bool(true);
    let resolved = env_val.resolve().await.unwrap();
    assert_eq!(resolved, "true");
}

#[tokio::test]
async fn test_resolve_with_policies_plain_string() {
    let env_val = EnvValue::WithPolicies(EnvVarWithPolicies {
        value: EnvValueSimple::String("policy_value".to_string()),
        policies: None,
    });
    let resolved = env_val.resolve().await.unwrap();
    assert_eq!(resolved, "policy_value");
}

// ==========================================================================
// Interpolation tests
// ==========================================================================

#[test]
fn test_env_part_literal() {
    let part = EnvPart::Literal("hello".to_string());
    assert!(!part.is_secret());
}

#[test]
fn test_env_part_secret() {
    let secret = crate::secrets::Secret::new("echo".to_string(), vec!["test".to_string()]);
    let part = EnvPart::Secret(secret);
    assert!(part.is_secret());
}

#[test]
fn test_env_part_deserialization_literal() {
    let json = r#""hello""#;
    let part: EnvPart = serde_json::from_str(json).unwrap();
    assert!(matches!(part, EnvPart::Literal(ref s) if s == "hello"));
    assert!(!part.is_secret());
}

#[test]
fn test_env_part_deserialization_secret() {
    let json = r#"{"resolver": "exec", "command": "echo", "args": ["test"]}"#;
    let part: EnvPart = serde_json::from_str(json).unwrap();
    assert!(part.is_secret());
}

#[test]
fn test_env_value_interpolated_deserialization() {
    let json = r#"["prefix-", {"resolver": "exec", "command": "gh", "args": ["auth", "token"]}]"#;
    let value: EnvValue = serde_json::from_str(json).unwrap();
    assert!(matches!(value, EnvValue::Interpolated(_)));
    assert!(value.is_secret());
}

#[test]
fn test_interpolated_is_secret_with_no_secrets() {
    let parts = vec![
        EnvPart::Literal("hello".to_string()),
        EnvPart::Literal("world".to_string()),
    ];
    let value = EnvValue::Interpolated(parts);
    assert!(!value.is_secret());
}

#[test]
fn test_interpolated_is_secret_with_secret() {
    let secret = crate::secrets::Secret::new("echo".to_string(), vec![]);
    let parts = vec![
        EnvPart::Literal("prefix".to_string()),
        EnvPart::Secret(secret),
    ];
    let value = EnvValue::Interpolated(parts);
    assert!(value.is_secret());
}

#[test]
fn test_interpolated_to_string_value_redacts_secrets() {
    let secret = crate::secrets::Secret::new(
        "gh".to_string(),
        vec!["auth".to_string(), "token".to_string()],
    );
    let parts = vec![
        EnvPart::Literal("access-tokens = github.com=".to_string()),
        EnvPart::Secret(secret),
    ];
    let value = EnvValue::Interpolated(parts);
    assert_eq!(value.to_string_value(), "access-tokens = github.com=*_*");
}

#[test]
fn test_interpolated_to_string_value_no_secrets() {
    let parts = vec![
        EnvPart::Literal("hello".to_string()),
        EnvPart::Literal("-".to_string()),
        EnvPart::Literal("world".to_string()),
    ];
    let value = EnvValue::Interpolated(parts);
    assert_eq!(value.to_string_value(), "hello-world");
}

#[tokio::test]
async fn test_resolve_with_secrets_collects_only_secret_parts() {
    // Test that only actual secret values are collected for redaction,
    // not the full interpolated string (when there are no secrets)
    let parts = vec![
        EnvPart::Literal("hello-".to_string()),
        EnvPart::Literal("world".to_string()),
    ];
    let value = EnvValue::Interpolated(parts);
    let (resolved, secrets) = value.resolve_with_secrets().await.unwrap();
    assert_eq!(resolved, "hello-world");
    assert!(secrets.is_empty()); // No secrets to redact
}

#[tokio::test]
async fn test_resolve_interpolated_concatenates_parts() {
    let parts = vec![
        EnvPart::Literal("a".to_string()),
        EnvPart::Literal("b".to_string()),
        EnvPart::Literal("c".to_string()),
    ];
    let value = EnvValue::Interpolated(parts);
    let resolved = value.resolve().await.unwrap();
    assert_eq!(resolved, "abc");
}

#[test]
fn test_interpolated_with_policies_is_secret() {
    let secret = crate::secrets::Secret::new("cmd".to_string(), vec![]);
    let parts = vec![
        EnvPart::Literal("prefix".to_string()),
        EnvPart::Secret(secret),
    ];

    let value = EnvValue::WithPolicies(EnvVarWithPolicies {
        value: EnvValueSimple::Interpolated(parts),
        policies: Some(vec![Policy {
            allow_tasks: Some(vec!["deploy".to_string()]),
            allow_exec: None,
        }]),
    });

    assert!(value.is_secret());
}

#[test]
fn test_interpolated_with_policies_to_string_value() {
    let secret = crate::secrets::Secret::new("cmd".to_string(), vec![]);
    let parts = vec![
        EnvPart::Literal("before-".to_string()),
        EnvPart::Secret(secret),
        EnvPart::Literal("-after".to_string()),
    ];

    let value = EnvValue::WithPolicies(EnvVarWithPolicies {
        value: EnvValueSimple::Interpolated(parts),
        policies: None,
    });

    assert_eq!(value.to_string_value(), "before-*_*-after");
}

#[test]
fn test_interpolated_accessible_by_task() {
    let parts = vec![EnvPart::Literal("value".to_string())];
    let value = EnvValue::Interpolated(parts);
    // Interpolated values without policies are always accessible
    assert!(value.is_accessible_by_task("any_task"));
}

#[test]
fn test_extract_static_env_vars_skips_interpolated_secrets() {
    // Simulate the extract_static_env_vars logic
    let secret = crate::secrets::Secret::new("cmd".to_string(), vec![]);
    let parts = vec![
        EnvPart::Literal("prefix".to_string()),
        EnvPart::Secret(secret),
    ];

    let mut base = HashMap::new();
    base.insert("PLAIN".to_string(), EnvValue::String("value".to_string()));
    base.insert(
        "INTERPOLATED_SECRET".to_string(),
        EnvValue::Interpolated(parts),
    );
    base.insert(
        "INTERPOLATED_PLAIN".to_string(),
        EnvValue::Interpolated(vec![
            EnvPart::Literal("a".to_string()),
            EnvPart::Literal("b".to_string()),
        ]),
    );

    // Filter out secrets (simulating extract_static_env_vars logic)
    let vars: HashMap<_, _> = base
        .iter()
        .filter(|(_, v)| !v.is_secret())
        .map(|(k, v)| (k.clone(), v.to_string_value()))
        .collect();

    assert!(vars.contains_key("PLAIN"));
    assert!(!vars.contains_key("INTERPOLATED_SECRET"));
    assert!(vars.contains_key("INTERPOLATED_PLAIN"));
    assert_eq!(vars.get("INTERPOLATED_PLAIN"), Some(&"ab".to_string()));
}

#[test]
fn test_env_value_simple_interpolated_deserialization() {
    // Test that EnvValueSimple can deserialize interpolated arrays
    let json = r#"["a", "b", "c"]"#;
    let value: EnvValueSimple = serde_json::from_str(json).unwrap();
    assert!(matches!(value, EnvValueSimple::Interpolated(_)));
}

#[test]
fn test_env_value_with_policies_interpolated_deserialization() {
    let json = r#"{
            "value": ["prefix-", {"resolver": "exec", "command": "gh", "args": ["auth", "token"]}],
            "policies": [{"allowTasks": ["deploy"]}]
        }"#;
    let value: EnvValue = serde_json::from_str(json).unwrap();
    assert!(matches!(value, EnvValue::WithPolicies(_)));
    assert!(value.is_secret());
}

#[test]
fn test_interpolated_empty_array() {
    let parts = vec![];
    let value = EnvValue::Interpolated(parts);
    assert_eq!(value.to_string_value(), "");
    assert!(!value.is_secret());
}

#[tokio::test]
async fn test_resolve_interpolated_with_actual_secret() {
    let secret = crate::secrets::Secret::new("echo".to_string(), vec!["secret_value".to_string()]);
    let parts = vec![
        EnvPart::Literal("prefix-".to_string()),
        EnvPart::Secret(secret),
        EnvPart::Literal("-suffix".to_string()),
    ];
    let value = EnvValue::Interpolated(parts);
    let (resolved, secrets) = value.resolve_with_secrets().await.unwrap();

    assert!(resolved.contains("prefix-"));
    assert!(resolved.contains("secret_value"));
    assert!(resolved.contains("-suffix"));
    assert_eq!(secrets.len(), 1);
}
