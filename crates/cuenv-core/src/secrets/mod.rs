//! Secret and resolver types
//!
//! Based on schema/secrets.cue

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// Resolver for executing commands to retrieve secret values
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct ExecResolver {
    /// Command to execute
    pub command: String,

    /// Arguments to pass to the command
    pub args: Vec<String>,
}

/// Secret definition with resolver
/// This is the base type that can be extended in CUE
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, PartialEq)]
pub struct Secret {
    /// Resolver type (currently only "exec" is supported)
    pub resolver: String,

    /// Command to execute
    pub command: String,

    /// Arguments to pass to the command
    #[serde(default)]
    pub args: Vec<String>,

    /// Additional fields for extensibility
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

impl Secret {
    /// Create a new secret with a resolver
    pub fn new(command: String, args: Vec<String>) -> Self {
        Secret {
            resolver: "exec".to_string(),
            command,
            args,
            extra: HashMap::new(),
        }
    }

    /// Create a secret with additional fields
    pub fn with_extra(command: String, args: Vec<String>, extra: HashMap<String, Value>) -> Self {
        Secret {
            resolver: "exec".to_string(),
            command,
            args,
            extra,
        }
    }
}
