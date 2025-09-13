#[cfg(test)]
mod tests {
    use crate::config::{CacheMode, Config, OutputFormat};
    use crate::environment::{Env, EnvValue};
    use crate::manifest::{Cuenv, HookList};
    use crate::secrets::Secret;
    use schemars::schema_for;

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
            "resolver": {
                "command": "op",
                "args": ["read", "secret"]
            }
        }))
        .unwrap();
        assert!(matches!(val, EnvValue::Secret(_)));
    }

    #[test]
    fn test_hook_list_variants() {
        use serde_json::json;

        // Test single hook
        let single: HookList = serde_json::from_value(json!({
            "command": "echo",
            "args": ["hello"]
        }))
        .unwrap();
        assert_eq!(single.to_vec().len(), 1);

        // Test multiple hooks
        let multiple: HookList = serde_json::from_value(json!([
            {"command": "echo", "args": ["hello"]},
            {"command": "echo", "args": ["world"]}
        ]))
        .unwrap();
        assert_eq!(multiple.to_vec().len(), 2);
    }
}
