//! CUE Cube loading and evaluation
//!
//! This module handles loading CUE Cubes and evaluating them to extract
//! file definitions. A "Cube" is a CUE-based template that defines multiple
//! files to generate for a project.

use crate::{CodegenError, Result};
use cuengine::ModuleEvalOptions;
use cuenv_core::ModuleEvaluation;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// File generation mode
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileMode {
    /// Always regenerate this file (managed by codegen)
    #[default]
    Managed,
    /// Generate only if file doesn't exist (user owns this file)
    Scaffold,
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

/// A project file definition from the cube
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectFileDefinition {
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
    /// Whether to add this file path to .gitignore
    #[serde(default)]
    pub gitignore: bool,
}

/// A CUE Cube containing file definitions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CubeData {
    /// Map of file paths to their definitions
    pub files: HashMap<String, ProjectFileDefinition>,
    /// Optional context data
    #[serde(default)]
    pub context: serde_json::Value,
}

/// CUE Cube loader and evaluator
///
/// A Cube is a CUE-based template that generates multiple project files.
/// Think of it as a 3D blueprint - each face of the cube represents
/// different aspects of your project (source code, config, tests, etc.)
#[derive(Debug)]
pub struct Cube {
    /// The cube data containing file definitions
    pub data: CubeData,
    /// Path to the source CUE file
    pub source_path: PathBuf,
}

impl Cube {
    /// Load a cube from a CUE file
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

    /// Get the file definitions from this cube
    #[must_use]
    pub fn files(&self) -> &HashMap<String, ProjectFileDefinition> {
        &self.data.files
    }

    /// Get the context data
    #[must_use]
    pub fn context(&self) -> &serde_json::Value {
        &self.data.context
    }

    /// Get the source path of this cube
    #[must_use]
    pub fn source_path(&self) -> &Path {
        &self.source_path
    }

    /// Evaluate a CUE file and extract the cube data
    fn evaluate_cue(path: &Path) -> Result<CubeData> {
        // Verify the file exists
        if !path.exists() {
            return Err(CodegenError::Cube(format!(
                "Cube file not found: {}",
                path.display()
            )));
        }

        // Determine the directory and package name from the path
        let dir_path = path.parent().ok_or_else(|| {
            CodegenError::Cube("Invalid cube path: no parent directory".to_string())
        })?;

        // Determine package name - try to infer from file content or use default
        let package_name = Self::determine_package_name(path)?;

        // Find the module root
        let module_root = Self::find_cue_module_root(dir_path).ok_or_else(|| {
            CodegenError::Cube(format!(
                "No CUE module found (looking for cue.mod/) starting from: {}",
                dir_path.display()
            ))
        })?;

        // Use module-wide evaluation
        let options = ModuleEvalOptions {
            recursive: true,
            ..Default::default()
        };
        let raw_result = cuengine::evaluate_module(&module_root, &package_name, Some(options))
            .map_err(|e| CodegenError::Cube(format!("CUE evaluation failed: {e}")))?;

        let module = ModuleEvaluation::from_raw(
            module_root.clone(),
            raw_result.instances,
            raw_result.projects,
        );

        // Calculate relative path and get the instance
        let target_path = dir_path
            .canonicalize()
            .map_err(|e| CodegenError::Cube(format!("Failed to canonicalize path: {e}")))?;
        let relative_path = target_path
            .strip_prefix(&module_root)
            .map(|p| {
                if p.as_os_str().is_empty() {
                    PathBuf::from(".")
                } else {
                    p.to_path_buf()
                }
            })
            .unwrap_or_else(|_| PathBuf::from("."));

        let instance = module.get(&relative_path).ok_or_else(|| {
            CodegenError::Cube(format!(
                "No CUE instance found at path: {} (relative: {})",
                dir_path.display(),
                relative_path.display()
            ))
        })?;

        instance
            .deserialize()
            .map_err(|e| CodegenError::Cube(format!("Failed to deserialize cube data: {e}")))
    }

    /// Find the CUE module root by walking up from `start` looking for `cue.mod/` directory.
    fn find_cue_module_root(start: &Path) -> Option<PathBuf> {
        let mut current = start.canonicalize().ok()?;
        loop {
            if current.join("cue.mod").is_dir() {
                return Some(current);
            }
            if !current.pop() {
                return None;
            }
        }
    }

    /// Determine the CUE package name from a file
    ///
    /// Reads the first few lines of the file to find a `package` declaration.
    /// Falls back to "cubes" if not found.
    fn determine_package_name(path: &Path) -> Result<String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| CodegenError::Cube(format!("Failed to read cube file: {e}")))?;

        // Look for package declaration in the first few lines
        for line in content.lines().take(10) {
            let trimmed = line.trim();
            if trimmed.starts_with("package ") {
                // Extract package name
                let package_name = trimmed.strip_prefix("package ").unwrap_or("cubes").trim();
                return Ok(package_name.to_string());
            }
        }

        // Default to "cubes" if no package declaration found
        Ok("cubes".to_string())
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
