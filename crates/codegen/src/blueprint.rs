//! Blueprint loading and evaluation
//!
//! This module handles loading CUE blueprints and evaluating them to extract
//! file definitions.

use crate::{CodegenError, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// File generation mode
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileMode {
    /// Always regenerate this file (managed by codegen)
    Managed,
    /// Generate only if file doesn't exist (user owns this file)
    Scaffold,
}

impl Default for FileMode {
    fn default() -> Self {
        Self::Managed
    }
}

/// Format configuration for a code file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormatConfig {
    /// Indent style: "space" or "tab"
    pub indent: String,
    /// Indent size (number of spaces or tab width)
    #[serde(rename = "indentSize")]
    pub indent_size: Option<usize>,
    /// Maximum line width
    #[serde(rename = "lineWidth")]
    pub line_width: Option<usize>,
    /// Trailing comma style
    #[serde(rename = "trailingComma")]
    pub trailing_comma: Option<String>,
    /// Use semicolons
    pub semicolons: Option<bool>,
    /// Quote style: "single" or "double"
    pub quotes: Option<String>,
}

impl Default for FormatConfig {
    fn default() -> Self {
        Self {
            indent: "space".to_string(),
            indent_size: Some(2),
            line_width: Some(100),
            trailing_comma: None,
            semicolons: None,
            quotes: None,
        }
    }
}

/// A file definition from the blueprint
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDefinition {
    /// Path where the file should be written
    pub path: String,
    /// Content of the file
    pub content: String,
    /// Programming language of the file
    pub language: String,
    /// Generation mode (managed or scaffold)
    #[serde(default)]
    pub mode: FileMode,
    /// Formatting configuration
    #[serde(default)]
    pub format: FormatConfig,
}

/// A CUE blueprint containing file definitions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlueprintData {
    /// Map of file paths to their definitions
    pub files: HashMap<String, FileDefinition>,
    /// Optional context data
    #[serde(default)]
    pub context: serde_json::Value,
}

/// Blueprint loader and evaluator
#[derive(Debug)]
pub struct Blueprint {
    data: BlueprintData,
    source_path: PathBuf,
}

impl Blueprint {
    /// Load a blueprint from a CUE file
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or the CUE evaluation fails
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();

        // For now, we'll use cuengine to evaluate the CUE file
        // This is a placeholder - the actual implementation will use cuengine
        let data = Self::evaluate_cue(path)?;

        Ok(Self {
            data,
            source_path: path.to_path_buf(),
        })
    }

    /// Get the file definitions from this blueprint
    #[must_use]
    pub fn files(&self) -> &HashMap<String, FileDefinition> {
        &self.data.files
    }

    /// Get the context data
    #[must_use]
    pub fn context(&self) -> &serde_json::Value {
        &self.data.context
    }

    /// Get the source path of this blueprint
    #[must_use]
    pub fn source_path(&self) -> &Path {
        &self.source_path
    }

    /// Evaluate a CUE file and extract the blueprint data
    fn evaluate_cue(path: &Path) -> Result<BlueprintData> {
        // This is a placeholder implementation
        // In the real implementation, we would:
        // 1. Run `cue export --out json path` to get JSON
        // 2. Parse the JSON into BlueprintData

        // For now, return a simple error if the file doesn't exist
        if !path.exists() {
            return Err(CodegenError::Blueprint(format!(
                "Blueprint file not found: {}",
                path.display()
            )));
        }

        // Read and parse the CUE file using cuengine
        // This is a simplified version - the real implementation will use cuengine properly
        let content = std::fs::read_to_string(path)
            .map_err(|e| CodegenError::Blueprint(format!("Failed to read blueprint: {}", e)))?;

        // For now, we'll expect the blueprint to be pre-evaluated to JSON
        // In a real implementation, we'd use cuengine to evaluate the CUE
        if content.trim_start().starts_with('{') {
            // It's JSON, parse it directly
            let data: BlueprintData = serde_json::from_str(&content)?;
            Ok(data)
        } else {
            Err(CodegenError::Blueprint(
                "CUE evaluation not yet implemented. Please provide JSON blueprint for now.".to_string()
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_mode_default() {
        assert_eq!(FileMode::default(), FileMode::Managed);
    }

    #[test]
    fn test_format_config_default() {
        let config = FormatConfig::default();
        assert_eq!(config.indent, "space");
        assert_eq!(config.indent_size, Some(2));
        assert_eq!(config.line_width, Some(100));
    }
}
