//! Environment management for cuenv
//!
//! This module handles environment variables from CUE configurations,
//! including extraction, propagation, and environment-specific overrides.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;

/// Policy for controlling environment variable access
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct Policy {
    /// Allowlist of task names that can access this variable
    #[serde(skip_serializing_if = "Option::is_none", rename = "allowTasks")]
    pub allow_tasks: Option<Vec<String>>,

    /// Allowlist of exec commands that can access this variable
    #[serde(skip_serializing_if = "Option::is_none", rename = "allowExec")]
    pub allow_exec: Option<Vec<String>>,
}

/// Environment variable with optional access policies
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct EnvVarWithPolicies {
    /// The actual value
    pub value: EnvValueSimple,

    /// Optional access policies
    #[serde(skip_serializing_if = "Option::is_none")]
    pub policies: Option<Vec<Policy>>,
}

/// Simple environment variable values (non-recursive)
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(untagged)]
pub enum EnvValueSimple {
    String(String),
    Int(i64),
    Bool(bool),
    Secret(crate::secrets::Secret),
}

/// Environment variable values can be strings, integers, booleans, secrets, or values with policies
/// When exported to actual environment, these will always be strings
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
#[serde(untagged)]
pub enum EnvValue {
    // Value with policies must come first for serde untagged to try it first
    WithPolicies(EnvVarWithPolicies),
    // Simple values (backward compatible)
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

impl EnvValue {
    /// Check if a task has access to this environment variable
    pub fn is_accessible_by_task(&self, task_name: &str) -> bool {
        match self {
            // Simple values are always accessible
            EnvValue::String(_) | EnvValue::Int(_) | EnvValue::Bool(_) | EnvValue::Secret(_) => {
                true
            }

            // Check policies for restricted variables
            EnvValue::WithPolicies(var) => match &var.policies {
                None => true,                                  // No policies means accessible
                Some(policies) if policies.is_empty() => true, // Empty policies means accessible
                Some(policies) => {
                    // Check if any policy allows this task
                    policies.iter().any(|policy| {
                        policy
                            .allow_tasks
                            .as_ref()
                            .is_some_and(|tasks| tasks.iter().any(|t| t == task_name))
                    })
                }
            },
        }
    }

    /// Check if an exec command has access to this environment variable
    pub fn is_accessible_by_exec(&self, command: &str) -> bool {
        match self {
            // Simple values are always accessible
            EnvValue::String(_) | EnvValue::Int(_) | EnvValue::Bool(_) | EnvValue::Secret(_) => {
                true
            }

            // Check policies for restricted variables
            EnvValue::WithPolicies(var) => match &var.policies {
                None => true,                                  // No policies means accessible
                Some(policies) if policies.is_empty() => true, // Empty policies means accessible
                Some(policies) => {
                    // Check if any policy allows this exec command
                    policies.iter().any(|policy| {
                        policy
                            .allow_exec
                            .as_ref()
                            .is_some_and(|execs| execs.iter().any(|e| e == command))
                    })
                }
            },
        }
    }

    /// Get the actual string value of the environment variable
    pub fn to_string_value(&self) -> String {
        match self {
            EnvValue::String(s) => s.clone(),
            EnvValue::Int(i) => i.to_string(),
            EnvValue::Bool(b) => b.to_string(),
            EnvValue::Secret(_) => "[SECRET]".to_string(), // Placeholder for secrets
            EnvValue::WithPolicies(var) => match &var.value {
                EnvValueSimple::String(s) => s.clone(),
                EnvValueSimple::Int(i) => i.to_string(),
                EnvValueSimple::Bool(b) => b.to_string(),
                EnvValueSimple::Secret(_) => "[SECRET]".to_string(),
            },
        }
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

    /// Build environment for a task, filtering based on policies
    pub fn build_for_task(
        task_name: &str,
        env_vars: &HashMap<String, EnvValue>,
    ) -> HashMap<String, String> {
        env_vars
            .iter()
            .filter(|(_, value)| value.is_accessible_by_task(task_name))
            .map(|(key, value)| (key.clone(), value.to_string_value()))
            .collect()
    }

    /// Build environment for exec command, filtering based on policies
    pub fn build_for_exec(
        command: &str,
        env_vars: &HashMap<String, EnvValue>,
    ) -> HashMap<String, String> {
        env_vars
            .iter()
            .filter(|(_, value)| value.is_accessible_by_exec(command))
            .map(|(key, value)| (key.clone(), value.to_string_value()))
            .collect()
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
}
