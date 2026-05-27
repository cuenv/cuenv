//! Task parameter definitions and resolved argument interpolation.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Task parameter definitions for CLI arguments
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TaskParams {
    /// Positional arguments (order matters, consumed left-to-right)
    /// Referenced in args as {{0}}, {{1}}, etc.
    #[serde(default)]
    pub positional: Vec<ParamDef>,

    /// Named arguments (--flag style) as direct fields
    /// Referenced in args as {{name}} where name matches the field name
    #[serde(flatten, default)]
    pub named: HashMap<String, ParamDef>,
}

/// Parameter type for validation
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ParamType {
    #[default]
    String,
    Bool,
    Int,
}

/// Parameter definition for task arguments
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ParamDef {
    /// Human-readable description shown in --help
    #[serde(default)]
    pub description: Option<String>,

    /// Whether the argument must be provided (default: false)
    #[serde(default)]
    pub required: bool,

    /// Default value if not provided
    #[serde(default)]
    pub default: Option<String>,

    /// Type hint for documentation (default: "string", not enforced at runtime)
    #[serde(default, rename = "type")]
    pub param_type: ParamType,

    /// Short flag (single character, e.g., "t" for -t)
    #[serde(default)]
    pub short: Option<String>,
}

/// Resolved task arguments ready for interpolation
#[derive(Debug, Clone, Default)]
pub struct ResolvedArgs {
    /// Positional argument values by index
    pub positional: Vec<String>,
    /// Named argument values by name
    pub named: HashMap<String, String>,
}

impl ResolvedArgs {
    /// Create empty resolved args
    pub fn new() -> Self {
        Self::default()
    }

    /// Interpolate placeholders in a string
    /// Supports {{0}}, {{1}} for positional and {{name}} for named args
    pub fn interpolate(&self, template: &str) -> String {
        let mut result = template.to_string();

        for (i, value) in self.positional.iter().enumerate() {
            let placeholder = format!("{{{{{}}}}}", i);
            result = result.replace(&placeholder, value);
        }

        for (name, value) in &self.named {
            let placeholder = format!("{{{{{}}}}}", name);
            result = result.replace(&placeholder, value);
        }

        result
    }

    /// Interpolate all args in a list
    pub fn interpolate_args(&self, args: &[String]) -> Vec<String> {
        args.iter().map(|arg| self.interpolate(arg)).collect()
    }
}
