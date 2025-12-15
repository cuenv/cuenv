//! File generation engine
//!
//! This module handles the core file generation logic, including:
//! - Writing files based on mode (managed vs scaffold)
//! - Formatting generated code
//! - Checking if files need updates

use crate::cube::{Cube, FileMode};
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
    cube: Cube,
    formatter: Formatter,
}

impl Generator {
    /// Create a new generator from a cube
    #[must_use]
    pub fn new(cube: Cube) -> Self {
        Self {
            cube,
            formatter: Formatter::new(),
        }
    }

    /// Generate all files from the cube
    ///
    /// # Errors
    ///
    /// Returns an error if file writing or formatting fails
    pub fn generate(&self, options: &GenerateOptions) -> Result<Vec<GeneratedFile>> {
        let mut generated_files = Vec::new();

        for (file_path, file_def) in self.cube.files() {
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
    use crate::cube::{CubeData, FileDefinition, FormatConfig};
    use std::collections::HashMap;
    use tempfile::TempDir;

    fn create_test_cube() -> Cube {
        let mut files = HashMap::new();
        files.insert(
            "test.json".to_string(),
            FileDefinition {
                content: r#"{"name":"test"}"#.to_string(),
                language: "json".to_string(),
                mode: FileMode::Managed,
                format: FormatConfig::default(),
            },
        );

        let data = CubeData {
            files,
            context: serde_json::Value::Null,
        };

        Cube {
            data,
            source_path: PathBuf::from("test.cue"),
        }
    }

    #[test]
    fn test_generator_new() {
        let cube = create_test_cube();
        let generator = Generator::new(cube);
        assert!(generator.cube.files().contains_key("test.json"));
    }

    #[test]
    fn test_generate_managed_file() {
        let cube = create_test_cube();
        let generator = Generator::new(cube);

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
}
