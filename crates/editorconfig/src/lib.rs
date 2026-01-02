//! Generate .editorconfig files from CUE configuration.
//!
//! This crate provides a builder-based API for generating `.editorconfig` files
//! from a declarative configuration.
//!
//! # Example
//!
//! ```rust,no_run
//! use cuenv_editorconfig::{EditorConfigFile, EditorConfigSection};
//!
//! let result = EditorConfigFile::builder()
//!     .directory(".")
//!     .is_root(true)
//!     .section("*", EditorConfigSection::new()
//!         .indent_style("space")
//!         .indent_size(4)
//!         .end_of_line("lf"))
//!     .section("*.md", EditorConfigSection::new()
//!         .trim_trailing_whitespace(false))
//!     .generate()?;
//!
//! println!("Status: {}", result.status);
//! # Ok::<(), cuenv_editorconfig::Error>(())
//! ```
//!
//! # Features
//!
//! - `serde`: Enable serde serialization/deserialization for configuration types

#![warn(missing_docs)]

use std::path::{Path, PathBuf};

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// A section in an EditorConfig file.
///
/// Each section corresponds to a glob pattern and contains settings for files
/// matching that pattern.
///
/// # Example
///
/// ```rust
/// use cuenv_editorconfig::EditorConfigSection;
///
/// let section = EditorConfigSection::new()
///     .indent_style("space")
///     .indent_size(4)
///     .end_of_line("lf")
///     .charset("utf-8")
///     .insert_final_newline(true)
///     .trim_trailing_whitespace(true);
/// ```
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct EditorConfigSection {
    indent_style: Option<String>,
    indent_size: Option<EditorConfigValue>,
    tab_width: Option<u32>,
    end_of_line: Option<String>,
    charset: Option<String>,
    trim_trailing_whitespace: Option<bool>,
    insert_final_newline: Option<bool>,
    max_line_length: Option<EditorConfigValue>,
}

/// A value that can be either an integer or a special string value.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "serde", serde(untagged))]
pub enum EditorConfigValue {
    /// Integer value
    Int(u32),
    /// String value (e.g., "tab" for indent_size, "off" for max_line_length)
    String(String),
}

impl EditorConfigValue {
    fn to_editorconfig_value(&self) -> String {
        match self {
            Self::Int(n) => n.to_string(),
            Self::String(s) => s.clone(),
        }
    }
}

impl EditorConfigSection {
    /// Create a new empty section.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the indentation style.
    ///
    /// Valid values: "tab", "space"
    #[must_use]
    pub fn indent_style(mut self, style: impl Into<String>) -> Self {
        self.indent_style = Some(style.into());
        self
    }

    /// Set the indentation style from an optional value.
    #[must_use]
    pub fn indent_style_opt(mut self, style: Option<impl Into<String>>) -> Self {
        self.indent_style = style.map(Into::into);
        self
    }

    /// Set the indentation size.
    #[must_use]
    pub fn indent_size(mut self, size: u32) -> Self {
        self.indent_size = Some(EditorConfigValue::Int(size));
        self
    }

    /// Set the indentation size to "tab".
    #[must_use]
    pub fn indent_size_tab(mut self) -> Self {
        self.indent_size = Some(EditorConfigValue::String("tab".to_string()));
        self
    }

    /// Set the indentation size from an optional value.
    #[must_use]
    pub fn indent_size_opt(mut self, size: Option<EditorConfigValue>) -> Self {
        self.indent_size = size;
        self
    }

    /// Set the tab width.
    #[must_use]
    pub fn tab_width(mut self, width: u32) -> Self {
        self.tab_width = Some(width);
        self
    }

    /// Set the tab width from an optional value.
    #[must_use]
    pub fn tab_width_opt(mut self, width: Option<u32>) -> Self {
        self.tab_width = width;
        self
    }

    /// Set the line ending style.
    ///
    /// Valid values: "lf", "crlf", "cr"
    #[must_use]
    pub fn end_of_line(mut self, eol: impl Into<String>) -> Self {
        self.end_of_line = Some(eol.into());
        self
    }

    /// Set the line ending style from an optional value.
    #[must_use]
    pub fn end_of_line_opt(mut self, eol: Option<impl Into<String>>) -> Self {
        self.end_of_line = eol.map(Into::into);
        self
    }

    /// Set the character encoding.
    ///
    /// Valid values: "utf-8", "utf-8-bom", "utf-16be", "utf-16le", "latin1"
    #[must_use]
    pub fn charset(mut self, charset: impl Into<String>) -> Self {
        self.charset = Some(charset.into());
        self
    }

    /// Set the character encoding from an optional value.
    #[must_use]
    pub fn charset_opt(mut self, charset: Option<impl Into<String>>) -> Self {
        self.charset = charset.map(Into::into);
        self
    }

    /// Set whether to trim trailing whitespace.
    #[must_use]
    pub fn trim_trailing_whitespace(mut self, trim: bool) -> Self {
        self.trim_trailing_whitespace = Some(trim);
        self
    }

    /// Set whether to trim trailing whitespace from an optional value.
    #[must_use]
    pub fn trim_trailing_whitespace_opt(mut self, trim: Option<bool>) -> Self {
        self.trim_trailing_whitespace = trim;
        self
    }

    /// Set whether to insert a final newline.
    #[must_use]
    pub fn insert_final_newline(mut self, insert: bool) -> Self {
        self.insert_final_newline = Some(insert);
        self
    }

    /// Set whether to insert a final newline from an optional value.
    #[must_use]
    pub fn insert_final_newline_opt(mut self, insert: Option<bool>) -> Self {
        self.insert_final_newline = insert;
        self
    }

    /// Set the maximum line length.
    #[must_use]
    pub fn max_line_length(mut self, length: u32) -> Self {
        self.max_line_length = Some(EditorConfigValue::Int(length));
        self
    }

    /// Set the maximum line length to "off".
    #[must_use]
    pub fn max_line_length_off(mut self) -> Self {
        self.max_line_length = Some(EditorConfigValue::String("off".to_string()));
        self
    }

    /// Set the maximum line length from an optional value.
    #[must_use]
    pub fn max_line_length_opt(mut self, length: Option<EditorConfigValue>) -> Self {
        self.max_line_length = length;
        self
    }

    /// Check if this section has any settings.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.indent_style.is_none()
            && self.indent_size.is_none()
            && self.tab_width.is_none()
            && self.end_of_line.is_none()
            && self.charset.is_none()
            && self.trim_trailing_whitespace.is_none()
            && self.insert_final_newline.is_none()
            && self.max_line_length.is_none()
    }

    /// Generate the content for this section (without the header).
    fn generate_content(&self) -> Vec<String> {
        let mut lines = Vec::new();

        if let Some(ref style) = self.indent_style {
            lines.push(format!("indent_style = {style}"));
        }
        if let Some(ref size) = self.indent_size {
            lines.push(format!("indent_size = {}", size.to_editorconfig_value()));
        }
        if let Some(width) = self.tab_width {
            lines.push(format!("tab_width = {width}"));
        }
        if let Some(ref eol) = self.end_of_line {
            lines.push(format!("end_of_line = {eol}"));
        }
        if let Some(ref charset) = self.charset {
            lines.push(format!("charset = {charset}"));
        }
        if let Some(trim) = self.trim_trailing_whitespace {
            lines.push(format!("trim_trailing_whitespace = {trim}"));
        }
        if let Some(insert) = self.insert_final_newline {
            lines.push(format!("insert_final_newline = {insert}"));
        }
        if let Some(ref length) = self.max_line_length {
            lines.push(format!(
                "max_line_length = {}",
                length.to_editorconfig_value()
            ));
        }

        lines
    }
}

/// Builder for generating an EditorConfig file.
///
/// # Example
///
/// ```rust,no_run
/// use cuenv_editorconfig::{EditorConfigFile, EditorConfigSection};
///
/// let result = EditorConfigFile::builder()
///     .directory("/path/to/project")
///     .is_root(true)
///     .section("*", EditorConfigSection::new()
///         .indent_style("space")
///         .indent_size(4))
///     .generate()?;
/// # Ok::<(), cuenv_editorconfig::Error>(())
/// ```
#[derive(Debug, Default)]
pub struct EditorConfigFileBuilder {
    directory: Option<PathBuf>,
    is_root: bool,
    sections: Vec<(String, EditorConfigSection)>,
    dry_run: bool,
    header: Option<String>,
}

/// Entry point for building and generating EditorConfig files.
pub struct EditorConfigFile;

impl EditorConfigFile {
    /// Create a new builder for generating an EditorConfig file.
    #[must_use]
    pub fn builder() -> EditorConfigFileBuilder {
        EditorConfigFileBuilder::default()
    }
}

impl EditorConfigFileBuilder {
    /// Set the directory where the .editorconfig file will be generated.
    ///
    /// Defaults to the current directory if not set.
    #[must_use]
    pub fn directory(mut self, dir: impl AsRef<Path>) -> Self {
        self.directory = Some(dir.as_ref().to_path_buf());
        self
    }

    /// Set whether this is the root .editorconfig file.
    ///
    /// When true, adds `root = true` at the top of the file, which tells
    /// editors to stop searching for .editorconfig files in parent directories.
    #[must_use]
    pub const fn is_root(mut self, is_root: bool) -> Self {
        self.is_root = is_root;
        self
    }

    /// Add a section to the EditorConfig file.
    ///
    /// Sections are output in the order they are added.
    #[must_use]
    pub fn section(mut self, pattern: impl Into<String>, section: EditorConfigSection) -> Self {
        self.sections.push((pattern.into(), section));
        self
    }

    /// Add multiple sections to the EditorConfig file.
    #[must_use]
    pub fn sections(
        mut self,
        sections: impl IntoIterator<Item = (impl Into<String>, EditorConfigSection)>,
    ) -> Self {
        self.sections
            .extend(sections.into_iter().map(|(p, s)| (p.into(), s)));
        self
    }

    /// Enable dry-run mode.
    ///
    /// When true, no files will be written. The result will indicate
    /// what would happen with `WouldCreate` and `WouldUpdate` statuses.
    #[must_use]
    pub const fn dry_run(mut self, dry_run: bool) -> Self {
        self.dry_run = dry_run;
        self
    }

    /// Set a header comment for the file.
    ///
    /// The header will be added at the top of the file with `#` prefixes.
    #[must_use]
    pub fn header(mut self, header: impl Into<String>) -> Self {
        self.header = Some(header.into());
        self
    }

    /// Generate the EditorConfig file.
    ///
    /// # Errors
    ///
    /// Returns an error if file I/O fails.
    pub fn generate(self) -> Result<SyncResult> {
        let dir = self.directory.clone().unwrap_or_else(|| PathBuf::from("."));
        let filepath = dir.join(".editorconfig");

        tracing::info!(
            path = %filepath.display(),
            is_root = self.is_root,
            sections = self.sections.len(),
            "Generating .editorconfig"
        );

        let content = self.generate_content();

        let status = if self.dry_run {
            determine_dry_run_status(&filepath, &content)?
        } else {
            write_file(&filepath, &content)?
        };

        tracing::info!(
            status = %status,
            "Processed .editorconfig"
        );

        Ok(SyncResult { status })
    }

    /// Generate the file content as a string.
    #[must_use]
    pub fn generate_content(&self) -> String {
        let mut lines = Vec::new();

        // Add header comment if present
        if let Some(ref header) = self.header {
            for line in header.lines() {
                lines.push(format!("# {line}"));
            }
            lines.push(String::new());
        }

        // Add root directive if this is the root file
        if self.is_root {
            lines.push("root = true".to_string());
            lines.push(String::new());
        }

        // Add sections
        for (pattern, section) in &self.sections {
            if section.is_empty() {
                continue;
            }

            lines.push(format!("[{pattern}]"));
            lines.extend(section.generate_content());
            lines.push(String::new());
        }

        // Remove trailing empty line and add final newline
        while lines.last().is_some_and(String::is_empty) {
            lines.pop();
        }

        if lines.is_empty() {
            String::new()
        } else {
            format!("{}\n", lines.join("\n"))
        }
    }
}

// ============================================================================
// Result types
// ============================================================================

/// Result of generating an EditorConfig file.
#[derive(Debug)]
pub struct SyncResult {
    /// The status of the file operation.
    pub status: FileStatus,
}

/// Status of a file operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStatus {
    /// File was newly created.
    Created,
    /// File existed and was updated with new content.
    Updated,
    /// File existed and content was unchanged.
    Unchanged,
    /// Would be created (dry-run mode).
    WouldCreate,
    /// Would be updated (dry-run mode).
    WouldUpdate,
}

impl std::fmt::Display for FileStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Created => write!(f, "Created"),
            Self::Updated => write!(f, "Updated"),
            Self::Unchanged => write!(f, "Unchanged"),
            Self::WouldCreate => write!(f, "Would create"),
            Self::WouldUpdate => write!(f, "Would update"),
        }
    }
}

// ============================================================================
// Error types
// ============================================================================

/// Errors that can occur during EditorConfig file generation.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// IO error during file operations.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Result type for EditorConfig operations.
pub type Result<T> = std::result::Result<T, Error>;

// ============================================================================
// Internal helpers
// ============================================================================

/// Determine what would happen to a file in dry-run mode.
fn determine_dry_run_status(filepath: &Path, content: &str) -> Result<FileStatus> {
    if !filepath.exists() {
        return Ok(FileStatus::WouldCreate);
    }
    let existing = std::fs::read_to_string(filepath)?;
    Ok(if existing == content {
        FileStatus::Unchanged
    } else {
        FileStatus::WouldUpdate
    })
}

/// Write a file and return the status.
fn write_file(filepath: &Path, content: &str) -> Result<FileStatus> {
    let status = if filepath.exists() {
        let existing = std::fs::read_to_string(filepath)?;
        if existing == content {
            return Ok(FileStatus::Unchanged);
        }
        FileStatus::Updated
    } else {
        FileStatus::Created
    };

    std::fs::write(filepath, content)?;
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_section_new() {
        let section = EditorConfigSection::new();
        assert!(section.is_empty());
    }

    #[test]
    fn test_section_builder() {
        let section = EditorConfigSection::new()
            .indent_style("space")
            .indent_size(4)
            .end_of_line("lf");

        assert!(!section.is_empty());
        let content = section.generate_content();
        assert!(content.contains(&"indent_style = space".to_string()));
        assert!(content.contains(&"indent_size = 4".to_string()));
        assert!(content.contains(&"end_of_line = lf".to_string()));
    }

    #[test]
    fn test_section_all_options() {
        let section = EditorConfigSection::new()
            .indent_style("tab")
            .indent_size(2)
            .tab_width(4)
            .end_of_line("crlf")
            .charset("utf-8")
            .trim_trailing_whitespace(true)
            .insert_final_newline(true)
            .max_line_length(120);

        let content = section.generate_content();
        assert_eq!(content.len(), 8);
    }

    #[test]
    fn test_section_tab_indent_size() {
        let section = EditorConfigSection::new().indent_size_tab();
        let content = section.generate_content();
        assert!(content.contains(&"indent_size = tab".to_string()));
    }

    #[test]
    fn test_section_max_line_length_off() {
        let section = EditorConfigSection::new().max_line_length_off();
        let content = section.generate_content();
        assert!(content.contains(&"max_line_length = off".to_string()));
    }

    #[test]
    fn test_builder_generate_content_root() {
        let content = EditorConfigFile::builder()
            .is_root(true)
            .section(
                "*",
                EditorConfigSection::new()
                    .indent_style("space")
                    .indent_size(4),
            )
            .generate_content();

        assert!(content.starts_with("root = true\n"));
        assert!(content.contains("[*]"));
        assert!(content.contains("indent_style = space"));
        assert!(content.contains("indent_size = 4"));
    }

    #[test]
    fn test_builder_generate_content_with_header() {
        let content = EditorConfigFile::builder()
            .header("Generated by cuenv")
            .section("*", EditorConfigSection::new().indent_style("space"))
            .generate_content();

        assert!(content.starts_with("# Generated by cuenv\n"));
    }

    #[test]
    fn test_builder_multiple_sections() {
        let content = EditorConfigFile::builder()
            .is_root(true)
            .section("*", EditorConfigSection::new().indent_style("space"))
            .section(
                "*.md",
                EditorConfigSection::new().trim_trailing_whitespace(false),
            )
            .section("Makefile", EditorConfigSection::new().indent_style("tab"))
            .generate_content();

        assert!(content.contains("[*]"));
        assert!(content.contains("[*.md]"));
        assert!(content.contains("[Makefile]"));
    }

    #[test]
    fn test_builder_empty_sections_skipped() {
        let content = EditorConfigFile::builder()
            .section("*", EditorConfigSection::new().indent_style("space"))
            .section("*.txt", EditorConfigSection::new()) // Empty, should be skipped
            .generate_content();

        assert!(content.contains("[*]"));
        assert!(!content.contains("[*.txt]"));
    }

    #[test]
    fn test_file_status_display() {
        assert_eq!(FileStatus::Created.to_string(), "Created");
        assert_eq!(FileStatus::Updated.to_string(), "Updated");
        assert_eq!(FileStatus::Unchanged.to_string(), "Unchanged");
        assert_eq!(FileStatus::WouldCreate.to_string(), "Would create");
        assert_eq!(FileStatus::WouldUpdate.to_string(), "Would update");
    }

    #[test]
    fn test_section_optional_builders_none() {
        let section = EditorConfigSection::new()
            .indent_style_opt(None::<String>)
            .indent_size_opt(None)
            .tab_width_opt(None)
            .end_of_line_opt(None::<String>)
            .charset_opt(None::<String>)
            .trim_trailing_whitespace_opt(None)
            .insert_final_newline_opt(None)
            .max_line_length_opt(None);

        assert!(section.is_empty());
    }

    #[test]
    fn test_section_optional_builders_some() {
        let section = EditorConfigSection::new()
            .indent_style_opt(Some("space"))
            .indent_size_opt(Some(EditorConfigValue::Int(4)))
            .tab_width_opt(Some(4))
            .end_of_line_opt(Some("lf"))
            .charset_opt(Some("utf-8"))
            .trim_trailing_whitespace_opt(Some(true))
            .insert_final_newline_opt(Some(true))
            .max_line_length_opt(Some(EditorConfigValue::Int(120)));

        let content = section.generate_content();
        assert_eq!(content.len(), 8);
    }

    #[test]
    fn test_editor_config_value_int() {
        let val = EditorConfigValue::Int(42);
        assert_eq!(val.to_editorconfig_value(), "42");
    }

    #[test]
    fn test_editor_config_value_string() {
        let val = EditorConfigValue::String("tab".to_string());
        assert_eq!(val.to_editorconfig_value(), "tab");
    }

    #[test]
    fn test_section_equality() {
        let s1 = EditorConfigSection::new()
            .indent_style("space")
            .indent_size(4);
        let s2 = EditorConfigSection::new()
            .indent_style("space")
            .indent_size(4);
        let s3 = EditorConfigSection::new()
            .indent_style("tab")
            .indent_size(4);

        assert_eq!(s1, s2);
        assert_ne!(s1, s3);
    }

    #[test]
    fn test_section_clone() {
        let original = EditorConfigSection::new()
            .indent_style("space")
            .charset("utf-8");
        let cloned = original.clone();
        assert_eq!(original, cloned);
    }

    #[test]
    fn test_section_debug() {
        let section = EditorConfigSection::new().indent_style("space");
        let debug = format!("{section:?}");
        assert!(debug.contains("EditorConfigSection"));
        assert!(debug.contains("space"));
    }

    #[test]
    fn test_builder_sections_method() {
        let sections = vec![
            ("*", EditorConfigSection::new().indent_style("space")),
            (
                "*.md",
                EditorConfigSection::new().trim_trailing_whitespace(false),
            ),
        ];

        let content = EditorConfigFile::builder()
            .sections(sections)
            .generate_content();

        assert!(content.contains("[*]"));
        assert!(content.contains("[*.md]"));
    }

    #[test]
    fn test_builder_empty_content() {
        let content = EditorConfigFile::builder().generate_content();
        assert!(content.is_empty());
    }

    #[test]
    fn test_builder_only_root() {
        let content = EditorConfigFile::builder().is_root(true).generate_content();
        assert_eq!(content, "root = true\n");
    }

    #[test]
    fn test_builder_directory() {
        let builder = EditorConfigFile::builder().directory("/tmp/test");
        let debug = format!("{builder:?}");
        assert!(debug.contains("/tmp/test"));
    }

    #[test]
    fn test_builder_dry_run() {
        let builder = EditorConfigFile::builder().dry_run(true);
        let debug = format!("{builder:?}");
        assert!(debug.contains("dry_run: true"));
    }

    #[test]
    fn test_multiline_header() {
        let content = EditorConfigFile::builder()
            .header("Line 1\nLine 2\nLine 3")
            .section("*", EditorConfigSection::new().indent_style("space"))
            .generate_content();

        assert!(content.contains("# Line 1\n"));
        assert!(content.contains("# Line 2\n"));
        assert!(content.contains("# Line 3\n"));
    }

    #[test]
    fn test_file_status_equality() {
        assert_eq!(FileStatus::Created, FileStatus::Created);
        assert_ne!(FileStatus::Created, FileStatus::Updated);
    }

    #[test]
    fn test_file_status_clone() {
        let status = FileStatus::Created;
        let cloned = status;
        assert_eq!(status, cloned);
    }

    #[test]
    fn test_file_status_debug() {
        let debug = format!("{:?}", FileStatus::WouldCreate);
        assert!(debug.contains("WouldCreate"));
    }

    #[test]
    fn test_sync_result_debug() {
        let result = SyncResult {
            status: FileStatus::Created,
        };
        let debug = format!("{result:?}");
        assert!(debug.contains("SyncResult"));
        assert!(debug.contains("Created"));
    }

    #[test]
    fn test_error_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let err: Error = io_err.into();
        let msg = err.to_string();
        assert!(msg.contains("IO error"));
    }

    #[test]
    fn test_error_debug() {
        let io_err = std::io::Error::new(std::io::ErrorKind::Other, "test error");
        let err: Error = io_err.into();
        let debug = format!("{err:?}");
        assert!(debug.contains("Io"));
    }

    #[test]
    fn test_generate_with_dry_run_would_create() {
        let temp = tempfile::TempDir::new().unwrap();
        let result = EditorConfigFile::builder()
            .directory(temp.path())
            .is_root(true)
            .section("*", EditorConfigSection::new().indent_style("space"))
            .dry_run(true)
            .generate()
            .unwrap();

        assert_eq!(result.status, FileStatus::WouldCreate);
        // File should not exist
        assert!(!temp.path().join(".editorconfig").exists());
    }

    #[test]
    fn test_generate_creates_file() {
        let temp = tempfile::TempDir::new().unwrap();
        let result = EditorConfigFile::builder()
            .directory(temp.path())
            .is_root(true)
            .section("*", EditorConfigSection::new().indent_style("space"))
            .generate()
            .unwrap();

        assert_eq!(result.status, FileStatus::Created);
        assert!(temp.path().join(".editorconfig").exists());
    }

    #[test]
    fn test_generate_unchanged() {
        let temp = tempfile::TempDir::new().unwrap();

        // First write
        let _ = EditorConfigFile::builder()
            .directory(temp.path())
            .is_root(true)
            .section("*", EditorConfigSection::new().indent_style("space"))
            .generate()
            .unwrap();

        // Second write with same content
        let result = EditorConfigFile::builder()
            .directory(temp.path())
            .is_root(true)
            .section("*", EditorConfigSection::new().indent_style("space"))
            .generate()
            .unwrap();

        assert_eq!(result.status, FileStatus::Unchanged);
    }

    #[test]
    fn test_generate_updated() {
        let temp = tempfile::TempDir::new().unwrap();

        // First write
        let _ = EditorConfigFile::builder()
            .directory(temp.path())
            .is_root(true)
            .section("*", EditorConfigSection::new().indent_style("space"))
            .generate()
            .unwrap();

        // Second write with different content
        let result = EditorConfigFile::builder()
            .directory(temp.path())
            .is_root(true)
            .section("*", EditorConfigSection::new().indent_style("tab"))
            .generate()
            .unwrap();

        assert_eq!(result.status, FileStatus::Updated);
    }
}
