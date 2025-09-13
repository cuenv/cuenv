//! Environment management for cuenv
//!
//! This module handles environment variables from CUE configurations,
//! including extraction, propagation, and environment-specific overrides.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;

/// Environment variable values can be strings, integers, booleans, or secrets
/// When exported to actual environment, these will always be strings
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(untagged)]
pub enum EnvValue {
    String(String),
    Int(i64),
    Bool(bool),
    Secret(crate::secrets::Secret),
}

/// Environment configuration with environment-specific overrides
/// Based on schema/env.cue
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq, Default)]
pub struct Env {
    /// Base environment variables
    /// Keys must match pattern: ^[A-Z][A-Z0-9_]*$
    #[serde(flatten)]
    pub base: HashMap<String, EnvValue>,

    /// Environment-specific overrides
    #[serde(skip_serializing_if = "Option::is_none")]
    pub environment: Option<HashMap<String, HashMap<String, EnvValue>>>,
}

impl Env {
    /// Get environment variables for a specific environment
    pub fn for_environment(&self, env_name: &str) -> HashMap<String, EnvValue> {
        let mut result = self.base.clone();

        if let Some(environments) = &self.environment
            && let Some(env_overrides) = environments.get(env_name)
        {
            result.extend(env_overrides.clone());
        }

        result
    }
}

/// Runtime environment variables for task execution
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Environment {
    /// Map of environment variable names to values
    #[serde(flatten)]
    pub vars: HashMap<String, String>,
}

impl Environment {
    /// Create a new empty environment
    pub fn new() -> Self {
        Self::default()
    }

    /// Create environment from a map
    pub fn from_map(vars: HashMap<String, String>) -> Self {
        Self { vars }
    }

    /// Get an environment variable value
    pub fn get(&self, key: &str) -> Option<&str> {
        self.vars.get(key).map(|s| s.as_str())
    }

    /// Set an environment variable
    pub fn set(&mut self, key: String, value: String) {
        self.vars.insert(key, value);
    }

    /// Check if an environment variable exists
    pub fn contains(&self, key: &str) -> bool {
        self.vars.contains_key(key)
    }

    /// Get all environment variables as a vector of key=value strings
    pub fn to_env_vec(&self) -> Vec<String> {
        self.vars
            .iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect()
    }

    /// Merge with system environment variables
    /// CUE environment variables take precedence
    pub fn merge_with_system(&self) -> HashMap<String, String> {
        let mut merged: HashMap<String, String> = env::vars().collect();

        // Override with CUE environment variables
        for (key, value) in &self.vars {
            merged.insert(key.clone(), value.clone());
        }

        merged
    }

    /// Convert to a vector of key=value strings including system environment
    pub fn to_full_env_vec(&self) -> Vec<String> {
        self.merge_with_system()
            .iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect()
    }

    /// Get the number of environment variables
    pub fn len(&self) -> usize {
        self.vars.len()
    }

    /// Check if the environment is empty
    pub fn is_empty(&self) -> bool {
        self.vars.is_empty()
    }

    /// Iterate over environment variables
    pub fn iter(&self) -> impl Iterator<Item = (&String, &String)> {
        self.vars.iter()
    }
}

#[cfg(test)]
mod tests {
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
}
