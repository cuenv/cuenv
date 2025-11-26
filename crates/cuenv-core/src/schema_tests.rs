#[cfg(test)]
mod tests {
    use crate::config::{CacheMode, Config, OutputFormat};
    use crate::environment::{Env, EnvValue};
    use crate::hooks::types::Hook;
    use crate::manifest::Cuenv;
    use crate::secrets::Secret;
    use schemars::schema_for;
    use std::collections::HashMap;

    #[test]
    fn test_config_schema_generation() {
        let schema = schema_for!(Config);
        let json = serde_json::to_string_pretty(&schema).unwrap();
        assert!(json.contains("\"title\": \"Config\""));
        assert!(json.contains("outputFormat"));
        assert!(json.contains("cacheMode"));
    }

    #[test]
    fn test_environment_schema_generation() {
        let schema = schema_for!(Env);
        let json = serde_json::to_string_pretty(&schema).unwrap();
        assert!(json.contains("\"title\": \"Env\""));
    }

    #[test]
    fn test_secret_schema_generation() {
        let schema = schema_for!(Secret);
        let json = serde_json::to_string_pretty(&schema).unwrap();
        assert!(json.contains("\"title\": \"Secret\""));
        assert!(json.contains("resolver"));
    }

    #[test]
    fn test_cuenv_schema_generation() {
        let schema = schema_for!(Cuenv);
        let json = serde_json::to_string_pretty(&schema).unwrap();
        assert!(json.contains("\"title\": \"Cuenv\""));
        assert!(json.contains("config"));
        assert!(json.contains("env"));
        assert!(json.contains("hooks"));
        assert!(json.contains("tasks"));
    }

    #[test]
    fn test_config_serialization() {
        let config = Config {
            output_format: Some(OutputFormat::Tui),
            cache_mode: Some(CacheMode::ReadWrite),
            cache_enabled: Some(true),
            audit_mode: None,
            trace_output: Some(false),
            default_environment: Some("dev".to_string()),
            default_capabilities: None,
        };

        let json = serde_json::to_string(&config).unwrap();
        let deserialized: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(config, deserialized);
    }

    #[test]
    fn test_environment_value_variants() {
        use serde_json::json;

        // Test string value
        let val: EnvValue = serde_json::from_value(json!("hello")).unwrap();
        assert!(matches!(val, EnvValue::String(s) if s == "hello"));

        // Test integer value
        let val: EnvValue = serde_json::from_value(json!(42)).unwrap();
        assert!(matches!(val, EnvValue::Int(42)));

        // Test boolean value
        let val: EnvValue = serde_json::from_value(json!(true)).unwrap();
        assert!(matches!(val, EnvValue::Bool(true)));

        // Test secret value
        let val: EnvValue = serde_json::from_value(json!({
            "resolver": "exec",
            "command": "op",
            "args": ["read", "secret"]
        }))
        .unwrap();
        assert!(matches!(val, EnvValue::Secret(_)));
    }

    #[test]
    fn test_hooks_map() {
        use serde_json::json;

        // Test single hook in map
        let hooks: HashMap<String, Hook> = serde_json::from_value(json!({
            "echo": {
                "command": "echo",
                "args": ["hello"]
            }
        }))
        .unwrap();
        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks.get("echo").unwrap().command, "echo");
        assert_eq!(hooks.get("echo").unwrap().order, 100); // default

        // Test multiple hooks in map
        let hooks: HashMap<String, Hook> = serde_json::from_value(json!({
            "nix": {"command": "nix", "order": 10, "propagate": true},
            "setup": {"command": "setup", "order": 50}
        }))
        .unwrap();
        assert_eq!(hooks.len(), 2);
        assert_eq!(hooks.get("nix").unwrap().order, 10);
        assert!(hooks.get("nix").unwrap().propagate);
        assert_eq!(hooks.get("setup").unwrap().order, 50);
        assert!(!hooks.get("setup").unwrap().propagate); // default false
    }
}
