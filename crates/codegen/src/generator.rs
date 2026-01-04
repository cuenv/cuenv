//! File generation engine
//!
//! This module handles the core file generation logic, including:
//! - Writing files based on mode (managed vs scaffold)
//! - Formatting generated code
//! - Checking if files need updates

use crate::codegen::{Codegen, FileMode};
use crate::formatter::Formatter;
use crate::{CodegenError, Result};
use std::path::{Path, PathBuf};

/// Generated file information
#[derive(Debug, Clone)]
pub struct GeneratedFile {
    /// Path where the file was/will be written
    pub path: PathBuf,
    /// Final content (after formatting)
    pub content: String,
    /// Generation mode
    pub mode: FileMode,
    /// Programming language
    pub language: String,
}

/// Options for file generation
#[derive(Debug, Clone)]
pub struct GenerateOptions {
    /// Output directory for generated files
    pub output_dir: PathBuf,
    /// Check mode: don't write files, just check if they would change
    pub check: bool,
    /// Show diffs for changed files
    pub diff: bool,
}

impl Default for GenerateOptions {
    fn default() -> Self {
        Self {
            output_dir: PathBuf::from("."),
            check: false,
            diff: false,
        }
    }
}

/// File generator
#[derive(Debug)]
pub struct Generator {
    codegen: Codegen,
    formatter: Formatter,
}

impl Generator {
    /// Create a new generator from a codegen configuration
    #[must_use]
    pub fn new(codegen: Codegen) -> Self {
        Self {
            codegen,
            formatter: Formatter::new(),
        }
    }

    /// Generate all files from the codegen configuration
    ///
    /// # Errors
    ///
    /// Returns an error if file writing or formatting fails
    pub fn generate(&self, options: &GenerateOptions) -> Result<Vec<GeneratedFile>> {
        let mut generated_files = Vec::new();

        for (file_path, file_def) in self.codegen.files() {
            let output_path = options.output_dir.join(file_path);

            // Format the content
            let formatted_content =
                self.formatter
                    .format(&file_def.content, &file_def.language, &file_def.format)?;

            let generated = GeneratedFile {
                path: output_path.clone(),
                content: formatted_content.clone(),
                mode: file_def.mode,
                language: file_def.language.clone(),
            };

            // Handle different modes
            match file_def.mode {
                FileMode::Managed => {
                    if options.check {
                        self.check_file(&output_path, &formatted_content)?;
                    } else {
                        self.write_file(&output_path, &formatted_content)?;
                    }
                }
                FileMode::Scaffold => {
                    if output_path.exists() {
                        tracing::info!("Skipping {} (scaffold mode, file exists)", file_path);
                    } else if options.check {
                        return Err(CodegenError::Generation(format!(
                            "Missing scaffold file: {file_path}"
                        )));
                    } else {
                        self.write_file(&output_path, &formatted_content)?;
                    }
                }
            }

            generated_files.push(generated);
        }

        Ok(generated_files)
    }

    /// Write a file to disk
    #[allow(clippy::unused_self)] // Will use self for write options in future
    fn write_file(&self, path: &Path, content: &str) -> Result<()> {
        // Create parent directories if they don't exist
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        std::fs::write(path, content)?;
        tracing::info!("Generated: {}", path.display());

        Ok(())
    }

    /// Check if a file would be modified
    #[allow(clippy::unused_self)] // Will use self for check options in future
    fn check_file(&self, path: &Path, expected_content: &str) -> Result<()> {
        if !path.exists() {
            return Err(CodegenError::Generation(format!(
                "Missing managed file: {}",
                path.display()
            )));
        }

        let actual_content = std::fs::read_to_string(path)?;

        if actual_content != expected_content {
            return Err(CodegenError::Generation(format!(
                "File would be modified: {}",
                path.display()
            )));
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codegen::{CodegenData, FormatConfig, ProjectFileDefinition};
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn create_test_codegen() -> Codegen {
        let mut files = HashMap::new();
        files.insert(
            "test.json".to_string(),
            ProjectFileDefinition {
                content: r#"{"name":"test"}"#.to_string(),
                language: "json".to_string(),
                mode: FileMode::Managed,
                format: FormatConfig::default(),
                gitignore: false,
            },
        );

        let data = CodegenData {
            files,
            context: serde_json::Value::Null,
        };

        Codegen {
            data,
            source_path: PathBuf::from("test.cue"),
        }
    }

    fn create_scaffold_codegen() -> Codegen {
        let mut files = HashMap::new();
        files.insert(
            "scaffold.txt".to_string(),
            ProjectFileDefinition {
                content: "scaffold content".to_string(),
                language: "text".to_string(),
                mode: FileMode::Scaffold,
                format: FormatConfig::default(),
                gitignore: false,
            },
        );

        let data = CodegenData {
            files,
            context: serde_json::Value::Null,
        };

        Codegen {
            data,
            source_path: PathBuf::from("scaffold.cue"),
        }
    }

    #[test]
    fn test_generate_options_default() {
        let options = GenerateOptions::default();
        assert_eq!(options.output_dir, PathBuf::from("."));
        assert!(!options.check);
        assert!(!options.diff);
    }

    #[test]
    fn test_generated_file_clone() {
        let file = GeneratedFile {
            path: PathBuf::from("test.rs"),
            content: "fn main() {}".to_string(),
            mode: FileMode::Managed,
            language: "rust".to_string(),
        };
        let cloned = file.clone();
        assert_eq!(cloned.path, file.path);
        assert_eq!(cloned.content, file.content);
        assert_eq!(cloned.mode, file.mode);
        assert_eq!(cloned.language, file.language);
    }

    #[test]
    fn test_generated_file_debug() {
        let file = GeneratedFile {
            path: PathBuf::from("test.rs"),
            content: "fn main() {}".to_string(),
            mode: FileMode::Managed,
            language: "rust".to_string(),
        };
        let debug = format!("{file:?}");
        assert!(debug.contains("test.rs"));
        assert!(debug.contains("rust"));
    }

    #[test]
    fn test_generator_new() {
        let codegen = create_test_codegen();
        let generator = Generator::new(codegen);
        assert!(generator.codegen.files().contains_key("test.json"));
    }

    #[test]
    fn test_generate_managed_file() {
        let codegen = create_test_codegen();
        let generator = Generator::new(codegen);

        let temp_dir = TempDir::new().unwrap();
        let options = GenerateOptions {
            output_dir: temp_dir.path().to_path_buf(),
            check: false,
            diff: false,
        };

        let result = generator.generate(&options);
        assert!(result.is_ok());

        let generated = result.unwrap();
        assert_eq!(generated.len(), 1);
        assert_eq!(generated[0].mode, FileMode::Managed);

        let file_path = temp_dir.path().join("test.json");
        assert!(file_path.exists());
    }

    #[test]
    fn test_generate_scaffold_creates_new_file() {
        let codegen = create_scaffold_codegen();
        let generator = Generator::new(codegen);

        let temp_dir = TempDir::new().unwrap();
        let options = GenerateOptions {
            output_dir: temp_dir.path().to_path_buf(),
            check: false,
            diff: false,
        };

        let result = generator.generate(&options);
        assert!(result.is_ok());

        let file_path = temp_dir.path().join("scaffold.txt");
        assert!(file_path.exists());
        let content = std::fs::read_to_string(file_path).unwrap();
        assert_eq!(content, "scaffold content");
    }

    #[test]
    fn test_generate_scaffold_skips_existing_file() {
        let codegen = create_scaffold_codegen();
        let generator = Generator::new(codegen);

        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("scaffold.txt");
        std::fs::write(&file_path, "existing content").unwrap();

        let options = GenerateOptions {
            output_dir: temp_dir.path().to_path_buf(),
            check: false,
            diff: false,
        };

        let result = generator.generate(&options);
        assert!(result.is_ok());

        // File should not be overwritten
        let content = std::fs::read_to_string(file_path).unwrap();
        assert_eq!(content, "existing content");
    }

    #[test]
    fn test_generate_check_mode_missing_managed_file() {
        let codegen = create_test_codegen();
        let generator = Generator::new(codegen);

        let temp_dir = TempDir::new().unwrap();
        let options = GenerateOptions {
            output_dir: temp_dir.path().to_path_buf(),
            check: true,
            diff: false,
        };

        let result = generator.generate(&options);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Missing managed file"));
    }

    #[test]
    fn test_generate_check_mode_file_would_be_modified() {
        let codegen = create_test_codegen();
        let generator = Generator::new(codegen);

        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.json");
        std::fs::write(&file_path, "different content").unwrap();

        let options = GenerateOptions {
            output_dir: temp_dir.path().to_path_buf(),
            check: true,
            diff: false,
        };

        let result = generator.generate(&options);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("would be modified"));
    }

    #[test]
    fn test_generate_check_mode_file_matches() {
        let codegen = create_test_codegen();
        let generator = Generator::new(codegen);

        let temp_dir = TempDir::new().unwrap();

        // First generate the file
        let options = GenerateOptions {
            output_dir: temp_dir.path().to_path_buf(),
            check: false,
            diff: false,
        };
        generator.generate(&options).unwrap();

        // Now check should pass
        let check_options = GenerateOptions {
            output_dir: temp_dir.path().to_path_buf(),
            check: true,
            diff: false,
        };
        let result = generator.generate(&check_options);
        assert!(result.is_ok());
    }

    #[test]
    fn test_generate_check_mode_missing_scaffold_file() {
        let codegen = create_scaffold_codegen();
        let generator = Generator::new(codegen);

        let temp_dir = TempDir::new().unwrap();
        let options = GenerateOptions {
            output_dir: temp_dir.path().to_path_buf(),
            check: true,
            diff: false,
        };

        let result = generator.generate(&options);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("Missing scaffold file"));
    }

    #[test]
    fn test_generate_creates_nested_directories() {
        let mut files = HashMap::new();
        files.insert(
            "deep/nested/path/file.txt".to_string(),
            ProjectFileDefinition {
                content: "nested content".to_string(),
                language: "text".to_string(),
                mode: FileMode::Managed,
                format: FormatConfig::default(),
                gitignore: false,
            },
        );

        let codegen = Codegen {
            data: CodegenData {
                files,
                context: serde_json::Value::Null,
            },
            source_path: PathBuf::from("test.cue"),
        };
        let generator = Generator::new(codegen);

        let temp_dir = TempDir::new().unwrap();
        let options = GenerateOptions {
            output_dir: temp_dir.path().to_path_buf(),
            check: false,
            diff: false,
        };

        let result = generator.generate(&options);
        assert!(result.is_ok());

        let file_path = temp_dir.path().join("deep/nested/path/file.txt");
        assert!(file_path.exists());
        let content = std::fs::read_to_string(file_path).unwrap();
        assert_eq!(content, "nested content");
    }

    #[test]
    fn test_generate_multiple_files() {
        let mut files = HashMap::new();
        files.insert(
            "file1.txt".to_string(),
            ProjectFileDefinition {
                content: "content 1".to_string(),
                language: "text".to_string(),
                mode: FileMode::Managed,
                format: FormatConfig::default(),
                gitignore: false,
            },
        );
        files.insert(
            "file2.txt".to_string(),
            ProjectFileDefinition {
                content: "content 2".to_string(),
                language: "text".to_string(),
                mode: FileMode::Scaffold,
                format: FormatConfig::default(),
                gitignore: false,
            },
        );

        let codegen = Codegen {
            data: CodegenData {
                files,
                context: serde_json::Value::Null,
            },
            source_path: PathBuf::from("test.cue"),
        };
        let generator = Generator::new(codegen);

        let temp_dir = TempDir::new().unwrap();
        let options = GenerateOptions {
            output_dir: temp_dir.path().to_path_buf(),
            check: false,
            diff: false,
        };

        let result = generator.generate(&options);
        assert!(result.is_ok());

        let generated = result.unwrap();
        assert_eq!(generated.len(), 2);

        assert!(temp_dir.path().join("file1.txt").exists());
        assert!(temp_dir.path().join("file2.txt").exists());
    }
}
