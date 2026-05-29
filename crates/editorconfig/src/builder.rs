use crate::{EditorConfigSection, Error, FileStatus, Result, SyncResult};
use std::path::{Path, PathBuf};

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
        validate_section_patterns(&self.sections)?;

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
    ///
    /// Crate-internal: callers must go through [`generate`](Self::generate),
    /// which validates section patterns before producing content. Exposing
    /// this directly would let unvalidated patterns emit malformed output.
    #[must_use]
    pub(crate) fn generate_content(&self) -> String {
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

fn validate_section_patterns(sections: &[(String, EditorConfigSection)]) -> Result<()> {
    sections
        .iter()
        .try_for_each(|(pattern, _)| validate_section_pattern(pattern))
}

// Keep these rules in sync with the key constraint in
// `schema/rules/editorconfig.cue` (regex `^[^\[\]\r\n]+$`), which enforces the
// same contract at CUE evaluation time.
fn validate_section_pattern(pattern: &str) -> Result<()> {
    if pattern.is_empty() {
        return Err(Error::InvalidSectionPattern {
            pattern: pattern.to_string(),
            reason: "pattern cannot be empty".to_string(),
        });
    }

    if pattern.contains('[') || pattern.contains(']') {
        return Err(Error::InvalidSectionPattern {
            pattern: pattern.to_string(),
            reason: "pattern cannot contain '[' or ']'".to_string(),
        });
    }

    if pattern.contains('\r') || pattern.contains('\n') {
        return Err(Error::InvalidSectionPattern {
            pattern: pattern.to_string(),
            reason: "pattern cannot contain line breaks".to_string(),
        });
    }

    Ok(())
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
    fn generate_rejects_empty_section_pattern() {
        let err = EditorConfigFile::builder()
            .section("", EditorConfigSection::new().indent_style("space"))
            .generate()
            .expect_err("empty section pattern should fail");

        assert!(
            err.to_string().contains("pattern cannot be empty"),
            "expected empty pattern validation error, got: {err}"
        );
    }

    #[test]
    fn generate_rejects_bracketed_section_pattern() {
        let err = EditorConfigFile::builder()
            .section("[*.rs]", EditorConfigSection::new().indent_style("space"))
            .generate()
            .expect_err("bracketed section pattern should fail");

        assert!(
            err.to_string().contains("cannot contain '[' or ']'"),
            "expected bracket validation error, got: {err}"
        );
    }

    #[test]
    fn generate_rejects_multiline_section_pattern() {
        let err = EditorConfigFile::builder()
            .section(
                "*.rs\n*.toml",
                EditorConfigSection::new().indent_style("space"),
            )
            .generate()
            .expect_err("multiline section pattern should fail");

        assert!(
            err.to_string().contains("cannot contain line breaks"),
            "expected line-break validation error, got: {err}"
        );
    }
}
