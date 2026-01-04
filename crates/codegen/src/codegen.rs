//! CUE Codegen loading and evaluation
//!
//! This module handles loading CUE codegen configurations and evaluating them to extract
//! file definitions. A Codegen configuration is a CUE-based template that defines multiple
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

/// A project file definition from the codegen configuration
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

/// CUE codegen data containing file definitions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodegenData {
    /// Map of file paths to their definitions
    pub files: HashMap<String, ProjectFileDefinition>,
    /// Optional context data
    #[serde(default)]
    pub context: serde_json::Value,
}

/// CUE Codegen loader and evaluator
///
/// A Codegen configuration is a CUE-based template that generates multiple project files.
/// It defines the structure and content of files to be generated for a project.
#[derive(Debug)]
pub struct Codegen {
    /// The codegen data containing file definitions
    pub data: CodegenData,
    /// Path to the source CUE file
    pub source_path: PathBuf,
}

impl Codegen {
    /// Load a codegen configuration from a CUE file
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

    /// Get the file definitions from this codegen configuration
    #[must_use]
    pub fn files(&self) -> &HashMap<String, ProjectFileDefinition> {
        &self.data.files
    }

    /// Get the context data
    #[must_use]
    pub fn context(&self) -> &serde_json::Value {
        &self.data.context
    }

    /// Get the source path of this codegen configuration
    #[must_use]
    pub fn source_path(&self) -> &Path {
        &self.source_path
    }

    /// Evaluate a CUE file and extract the codegen data
    fn evaluate_cue(path: &Path) -> Result<CodegenData> {
        // Verify the file exists
        if !path.exists() {
            return Err(CodegenError::Codegen(format!(
                "Codegen file not found: {}",
                path.display()
            )));
        }

        // Determine the directory and package name from the path
        let dir_path = path.parent().ok_or_else(|| {
            CodegenError::Codegen("Invalid codegen path: no parent directory".to_string())
        })?;

        // Determine package name - try to infer from file content or use default
        let package_name = Self::determine_package_name(path)?;

        // Find the module root
        let module_root = Self::find_cue_module_root(dir_path).ok_or_else(|| {
            CodegenError::Codegen(format!(
                "No CUE module found (looking for cue.mod/) starting from: {}",
                dir_path.display()
            ))
        })?;

        // Use module-wide evaluation
        let options = ModuleEvalOptions {
            recursive: true,
            ..Default::default()
        };
        let raw_result = cuengine::evaluate_module(&module_root, &package_name, Some(&options))
            .map_err(|e| CodegenError::Codegen(format!("CUE evaluation failed: {e}")))?;

        let module = ModuleEvaluation::from_raw(
            module_root.clone(),
            raw_result.instances,
            raw_result.projects,
        );

        // Calculate relative path and get the instance
        let target_path = dir_path
            .canonicalize()
            .map_err(|e| CodegenError::Codegen(format!("Failed to canonicalize path: {e}")))?;
        let relative_path = target_path.strip_prefix(&module_root).map_or_else(
            |_| PathBuf::from("."),
            |p| {
                if p.as_os_str().is_empty() {
                    PathBuf::from(".")
                } else {
                    p.to_path_buf()
                }
            },
        );

        let instance = module.get(&relative_path).ok_or_else(|| {
            CodegenError::Codegen(format!(
                "No CUE instance found at path: {} (relative: {})",
                dir_path.display(),
                relative_path.display()
            ))
        })?;

        instance
            .deserialize()
            .map_err(|e| CodegenError::Codegen(format!("Failed to deserialize codegen data: {e}")))
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
    /// Falls back to "codegen" if not found.
    fn determine_package_name(path: &Path) -> Result<String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| CodegenError::Codegen(format!("Failed to read codegen file: {e}")))?;

        // Look for package declaration in the first few lines
        for line in content.lines().take(10) {
            let trimmed = line.trim();
            if trimmed.starts_with("package ") {
                // Extract package name
                let package_name = trimmed.strip_prefix("package ").unwrap_or("codegen").trim();
                return Ok(package_name.to_string());
            }
        }

        // Default to "codegen" if no package declaration found
        Ok("codegen".to_string())
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
    fn test_file_mode_serde_managed() {
        let json = r#""managed""#;
        let mode: FileMode = serde_json::from_str(json).unwrap();
        assert_eq!(mode, FileMode::Managed);
        assert_eq!(serde_json::to_string(&mode).unwrap(), json);
    }

    #[test]
    fn test_file_mode_serde_scaffold() {
        let json = r#""scaffold""#;
        let mode: FileMode = serde_json::from_str(json).unwrap();
        assert_eq!(mode, FileMode::Scaffold);
        assert_eq!(serde_json::to_string(&mode).unwrap(), json);
    }

    #[test]
    fn test_file_mode_clone() {
        let mode = FileMode::Managed;
        let cloned = mode;
        assert_eq!(mode, cloned);
    }

    #[test]
    fn test_file_mode_copy() {
        let mode = FileMode::Scaffold;
        let copied = mode;
        assert_eq!(mode, copied);
    }

    #[test]
    fn test_format_config_default() {
        let config = FormatConfig::default();
        assert_eq!(config.indent, "space");
        assert_eq!(config.indent_size, Some(2));
        assert_eq!(config.line_width, Some(100));
        assert!(config.trailing_comma.is_none());
        assert!(config.semicolons.is_none());
        assert!(config.quotes.is_none());
    }

    #[test]
    fn test_format_config_clone() {
        let config = FormatConfig {
            indent: "tab".to_string(),
            indent_size: Some(4),
            line_width: Some(120),
            trailing_comma: Some("all".to_string()),
            semicolons: Some(true),
            quotes: Some("single".to_string()),
        };
        let cloned = config.clone();
        assert_eq!(cloned.indent, "tab");
        assert_eq!(cloned.indent_size, Some(4));
        assert_eq!(cloned.line_width, Some(120));
        assert_eq!(cloned.trailing_comma, Some("all".to_string()));
        assert_eq!(cloned.semicolons, Some(true));
        assert_eq!(cloned.quotes, Some("single".to_string()));
    }

    #[test]
    fn test_format_config_serde_roundtrip() {
        let config = FormatConfig {
            indent: "space".to_string(),
            indent_size: Some(2),
            line_width: Some(80),
            trailing_comma: Some("es5".to_string()),
            semicolons: Some(false),
            quotes: Some("double".to_string()),
        };
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: FormatConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(config.indent, deserialized.indent);
        assert_eq!(config.indent_size, deserialized.indent_size);
        assert_eq!(config.quotes, deserialized.quotes);
    }

    #[test]
    fn test_project_file_definition_serde() {
        let def = ProjectFileDefinition {
            content: "test content".to_string(),
            language: "json".to_string(),
            mode: FileMode::Scaffold,
            format: FormatConfig::default(),
            gitignore: true,
        };
        let json = serde_json::to_string(&def).unwrap();
        let deserialized: ProjectFileDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.content, "test content");
        assert_eq!(deserialized.language, "json");
        assert_eq!(deserialized.mode, FileMode::Scaffold);
        assert!(deserialized.gitignore);
    }

    #[test]
    fn test_project_file_definition_defaults() {
        // Test that serde default attributes work
        let json = r#"{"content":"x","language":"rust"}"#;
        let def: ProjectFileDefinition = serde_json::from_str(json).unwrap();
        assert_eq!(def.mode, FileMode::Managed); // default
        assert!(!def.gitignore); // default
    }

    #[test]
    fn test_codegen_data_serde() {
        let mut files = HashMap::new();
        files.insert(
            "test.rs".to_string(),
            ProjectFileDefinition {
                content: "fn main() {}".to_string(),
                language: "rust".to_string(),
                mode: FileMode::Managed,
                format: FormatConfig::default(),
                gitignore: false,
            },
        );
        let data = CodegenData {
            files,
            context: serde_json::json!({"key": "value"}),
        };
        let json = serde_json::to_string(&data).unwrap();
        let deserialized: CodegenData = serde_json::from_str(&json).unwrap();
        assert!(deserialized.files.contains_key("test.rs"));
        assert_eq!(deserialized.context["key"], "value");
    }

    #[test]
    fn test_codegen_data_default_context() {
        let json = r#"{"files":{}}"#;
        let data: CodegenData = serde_json::from_str(json).unwrap();
        assert!(data.files.is_empty());
        assert!(data.context.is_null());
    }

    #[test]
    fn test_codegen_accessors() {
        let mut files = HashMap::new();
        files.insert(
            "example.js".to_string(),
            ProjectFileDefinition {
                content: "console.log('hi')".to_string(),
                language: "javascript".to_string(),
                mode: FileMode::Managed,
                format: FormatConfig::default(),
                gitignore: false,
            },
        );
        let codegen = Codegen {
            data: CodegenData {
                files,
                context: serde_json::json!({"project": "test"}),
            },
            source_path: PathBuf::from("/path/to/codegen.cue"),
        };

        assert_eq!(codegen.files().len(), 1);
        assert!(codegen.files().contains_key("example.js"));
        assert_eq!(codegen.context()["project"], "test");
        assert_eq!(codegen.source_path(), Path::new("/path/to/codegen.cue"));
    }

    #[test]
    fn test_codegen_load_nonexistent_file() {
        let result = Codegen::load("/nonexistent/path/codegen.cue");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Codegen file not found"));
    }

    #[test]
    fn test_determine_package_name_finds_package() {
        use std::io::Write;
        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join("test.cue");
        let mut file = std::fs::File::create(&file_path).unwrap();
        writeln!(file, "// comment").unwrap();
        writeln!(file, "package mypackage").unwrap();
        writeln!(file).unwrap();
        writeln!(file, "data: 123").unwrap();

        let name = Codegen::determine_package_name(&file_path).unwrap();
        assert_eq!(name, "mypackage");
    }

    #[test]
    fn test_determine_package_name_defaults() {
        use std::io::Write;
        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join("test.cue");
        let mut file = std::fs::File::create(&file_path).unwrap();
        writeln!(file, "// no package declaration").unwrap();
        writeln!(file, "data: 123").unwrap();

        let name = Codegen::determine_package_name(&file_path).unwrap();
        assert_eq!(name, "codegen");
    }

    #[test]
    fn test_determine_package_name_file_not_found() {
        let result = Codegen::determine_package_name(Path::new("/nonexistent/file.cue"));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Failed to read codegen file"));
    }

    #[test]
    fn test_find_cue_module_root_found() {
        let temp_dir = tempfile::tempdir().unwrap();
        let cue_mod = temp_dir.path().join("cue.mod");
        std::fs::create_dir(&cue_mod).unwrap();
        let subdir = temp_dir.path().join("subdir");
        std::fs::create_dir(&subdir).unwrap();

        let root = Codegen::find_cue_module_root(&subdir);
        assert!(root.is_some());
        assert_eq!(root.unwrap(), temp_dir.path().canonicalize().unwrap());
    }

    #[test]
    fn test_find_cue_module_root_not_found() {
        let temp_dir = tempfile::tempdir().unwrap();
        let root = Codegen::find_cue_module_root(temp_dir.path());
        assert!(root.is_none());
    }

    #[test]
    fn test_codegen_load_no_cue_module() {
        use std::io::Write;
        let temp_dir = tempfile::tempdir().unwrap();
        let file_path = temp_dir.path().join("codegen.cue");
        let mut file = std::fs::File::create(&file_path).unwrap();
        writeln!(file, "package test").unwrap();
        writeln!(file, "files: {{}}").unwrap();

        let result = Codegen::load(&file_path);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("No CUE module found"));
    }
}
