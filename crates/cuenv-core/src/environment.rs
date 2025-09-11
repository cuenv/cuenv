//! Environment management for task execution
//!
//! This module handles extraction and propagation of environment variables
//! from CUE configurations to task execution contexts.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;

/// Reserved field name that should be skipped during environment variable extraction
/// This field may contain metadata rather than environment variables
const RESERVED_ENV_FIELD: &str = "environment";

/// Environment variables from CUE configuration
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

/// Combined CUE evaluation result with environment and tasks
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CueEvaluation {
    /// Raw env object from CUE (contains environment variables)
    #[serde(default)]
    pub env: serde_json::Value,
    
    /// Task definitions
    #[serde(default)]
    pub tasks: crate::task::Tasks,
}

impl CueEvaluation {
    /// Create a new evaluation result
    pub fn new() -> Self {
        Self::default()
    }
    
    /// Parse from JSON string
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
    
    /// Extract environment variables from the env object
    pub fn get_environment(&self) -> Environment {
        let mut env = Environment::new();
        
        // The env object contains environment variables as direct properties
        if let serde_json::Value::Object(map) = &self.env {
            for (key, value) in map {
                // Skip reserved fields that contain metadata
                if key == RESERVED_ENV_FIELD {
                    continue;
                }
                
                // Convert value to string
                let str_value = match value {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Number(n) => n.to_string(),
                    serde_json::Value::Bool(b) => b.to_string(),
                    serde_json::Value::Null => continue, // Skip null values
                    serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
                        // Serialize complex values to JSON strings
                        match serde_json::to_string(value) {
                            Ok(json_str) => json_str,
                            Err(_) => continue, // Skip if serialization fails
                        }
                    }
                };
                
                env.set(key.clone(), str_value);
            }
        }
        
        env
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
    fn test_cue_evaluation_deserialization() {
        let json = r#"{
            "env": {
                "NAME": "test",
                "VALUE": "42"
            },
            "tasks": {
                "greet": {
                    "command": "echo",
                    "args": ["hello"]
                }
            }
        }"#;
        
        let eval = CueEvaluation::from_json(json).unwrap();
        
        // Test environment extraction
        let env = eval.get_environment();
        assert_eq!(env.get("NAME"), Some("test"));
        assert_eq!(env.get("VALUE"), Some("42"));
        
        // Test tasks
        assert!(eval.tasks.contains("greet"));
    }
    
    #[test]
    fn test_cue_evaluation_empty() {
        let json = "{}";
        let eval = CueEvaluation::from_json(json).unwrap();
        
        let env = eval.get_environment();
        assert!(env.is_empty());
        assert!(eval.tasks.list_tasks().is_empty());
    }
    
    #[test]
    fn test_cue_evaluation_env_only() {
        let json = r#"{
            "env": {
                "FOO": "bar"
            }
        }"#;
        
        let eval = CueEvaluation::from_json(json).unwrap();
        
        let env = eval.get_environment();
        assert_eq!(env.get("FOO"), Some("bar"));
        assert!(eval.tasks.list_tasks().is_empty());
    }
    
    #[test]
    fn test_cue_evaluation_tasks_only() {
        let json = r#"{
            "tasks": {
                "test": {
                    "command": "test"
                }
            }
        }"#;
        
        let eval = CueEvaluation::from_json(json).unwrap();
        
        let env = eval.get_environment();
        assert!(env.is_empty());
        assert!(eval.tasks.contains("test"));
    }
}