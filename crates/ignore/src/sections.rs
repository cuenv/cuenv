//! Managed ignore-file section support.

use std::io::ErrorKind;
use std::path::Path;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

use crate::{
    Error, FileResult, FileStatus, Result, determine_dry_run_status, validate_filename,
    write_ignore_file,
};

const SECTION_BEGIN_PREFIX: &str = "# BEGIN ";
const SECTION_END_PREFIX: &str = "# END ";

/// A managed section inside an ignore file.
///
/// Sections are rendered with `# BEGIN {name}` and `# END {name}` markers.
/// They allow one provider to update its own block while preserving the rest
/// of the ignore file.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct IgnoreSection {
    pub(super) name: String,
    pub(super) filename: String,
    pub(super) patterns: Vec<String>,
}

impl IgnoreSection {
    /// Create a managed section for `.gitignore`.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            filename: ".gitignore".to_string(),
            patterns: Vec::new(),
        }
    }

    /// Set the ignore file that owns this section.
    #[must_use]
    pub fn filename(mut self, filename: impl Into<String>) -> Self {
        self.filename = filename.into();
        self
    }

    /// Add a single pattern to the section.
    #[must_use]
    pub fn pattern(mut self, pattern: impl Into<String>) -> Self {
        self.patterns.push(pattern.into());
        self
    }

    /// Add multiple patterns to the section.
    #[must_use]
    pub fn patterns(mut self, patterns: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.patterns.extend(patterns.into_iter().map(Into::into));
        self
    }

    /// Get the section name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Get the output filename for this section.
    #[must_use]
    pub fn output_filename(&self) -> &str {
        &self.filename
    }

    /// Get the patterns.
    #[must_use]
    pub fn patterns_list(&self) -> &[String] {
        &self.patterns
    }

    fn begin_marker(&self) -> String {
        format!("# BEGIN {}", self.name)
    }

    fn end_marker(&self) -> String {
        format!("# END {}", self.name)
    }

    fn generate(&self) -> String {
        let mut patterns = self.patterns.clone();
        patterns.sort();
        patterns.dedup();

        let lines = std::iter::once(self.begin_marker())
            .chain(patterns)
            .chain(std::iter::once(self.end_marker()))
            .collect::<Vec<_>>();
        format!("{}\n", lines.join("\n"))
    }
}

#[derive(Clone, Copy)]
pub enum WriteMode {
    DryRun,
    Write,
}

/// Process a single managed section and return its result.
pub fn process_ignore_section(
    dir: &Path,
    section: &IgnoreSection,
    mode: WriteMode,
) -> Result<FileResult> {
    validate_section_name(&section.name)?;
    validate_filename(&section.filename)?;

    let filepath = dir.join(&section.filename);
    let (existing, file_missing) = read_optional_ignore_file(&filepath)?;
    let content = apply_managed_section(&existing, section)?;
    if content.is_empty() && file_missing {
        return Ok(FileResult {
            filename: section.filename.clone(),
            status: FileStatus::Unchanged,
            pattern_count: section.patterns.len(),
        });
    }
    let status = determine_file_status_for_mode(&filepath, &content, mode)?;

    tracing::info!(
        filename = %section.filename,
        status = %status,
        patterns = section.patterns.len(),
        section = %section.name,
        "Processed ignore section"
    );

    Ok(FileResult {
        filename: section.filename.clone(),
        status,
        pattern_count: section.patterns.len(),
    })
}

fn determine_file_status_for_mode(
    filepath: &Path,
    content: &str,
    mode: WriteMode,
) -> Result<FileStatus> {
    match mode {
        WriteMode::DryRun => determine_dry_run_status(filepath, content),
        WriteMode::Write => write_ignore_file(filepath, content),
    }
}

fn read_optional_ignore_file(filepath: &Path) -> Result<(String, bool)> {
    match std::fs::symlink_metadata(filepath) {
        Ok(_) => Ok((std::fs::read_to_string(filepath)?, false)),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok((String::new(), true)),
        Err(err) => Err(err.into()),
    }
}

pub fn preserve_existing_sections(filepath: &Path, generated: &str) -> Result<String> {
    let Ok(existing) = std::fs::read_to_string(filepath) else {
        return Ok(generated.to_string());
    };
    let sections = extract_managed_sections(&existing)?;
    if sections.is_empty() {
        return Ok(generated.to_string());
    }

    let mut next = generated.trim_end().to_string();
    for section in sections {
        let begin_marker = format!("{SECTION_BEGIN_PREFIX}{}", section.name);
        if generated.lines().any(|line| line == begin_marker) {
            continue;
        }
        if !next.is_empty() {
            next.push_str("\n\n");
        }
        next.push_str(section.content.trim_end());
    }
    next.push('\n');
    Ok(next)
}

fn apply_managed_section(existing: &str, section: &IgnoreSection) -> Result<String> {
    let without_section = remove_managed_section(existing, &section.name)?;
    if section.patterns.is_empty() {
        return Ok(without_section);
    }

    let mut next = without_section.trim_end().to_string();
    if !next.is_empty() {
        next.push_str("\n\n");
    }
    next.push_str(section.generate().trim_end());
    next.push('\n');
    Ok(next)
}

#[derive(Debug)]
struct ManagedSectionBlock {
    name: String,
    content: String,
}

fn extract_managed_sections(content: &str) -> Result<Vec<ManagedSectionBlock>> {
    let mut sections = Vec::new();
    let mut current: Option<(String, Vec<String>)> = None;

    for line in content.lines() {
        if let Some(name) = line.strip_prefix(SECTION_BEGIN_PREFIX) {
            if current.is_some() {
                return Err(malformed_section("nested begin marker"));
            }
            validate_section_name(name)?;
            current = Some((name.to_string(), vec![line.to_string()]));
            continue;
        }

        if let Some(name) = line.strip_prefix(SECTION_END_PREFIX) {
            let Some((current_name, mut lines)) = current.take() else {
                return Err(malformed_section("end marker without begin marker"));
            };
            if name != current_name {
                return Err(malformed_section(format!(
                    "end marker '{}' does not match begin marker '{}'",
                    name, current_name
                )));
            }
            lines.push(line.to_string());
            sections.push(ManagedSectionBlock {
                name: current_name,
                content: format!("{}\n", lines.join("\n")),
            });
            continue;
        }

        if let Some((_, lines)) = current.as_mut() {
            lines.push(line.to_string());
        }
    }

    if current.is_some() {
        return Err(malformed_section("missing end marker"));
    }

    Ok(sections)
}

fn remove_managed_section(content: &str, section_name: &str) -> Result<String> {
    let mut output = Vec::new();
    let mut current: Option<(String, Vec<String>)> = None;

    for line in content.lines() {
        if let Some(name) = line.strip_prefix(SECTION_BEGIN_PREFIX) {
            if current.is_some() {
                return Err(malformed_section("nested begin marker"));
            }
            validate_section_name(name)?;
            current = Some((name.to_string(), vec![line.to_string()]));
            continue;
        }

        if let Some(name) = line.strip_prefix(SECTION_END_PREFIX) {
            let Some((current_name, mut lines)) = current.take() else {
                return Err(malformed_section("end marker without begin marker"));
            };
            if name != current_name {
                return Err(malformed_section(format!(
                    "end marker '{}' does not match begin marker '{}'",
                    name, current_name
                )));
            }
            lines.push(line.to_string());
            if current_name != section_name {
                output.extend(lines);
            }
            continue;
        }

        if let Some((_, lines)) = current.as_mut() {
            lines.push(line.to_string());
        } else {
            output.push(line.to_string());
        }
    }

    if current.is_some() {
        return Err(malformed_section("missing end marker"));
    }

    let mut joined = output.join("\n");
    if content.ends_with('\n') && !joined.is_empty() {
        joined.push('\n');
    }
    Ok(joined)
}

fn malformed_section(reason: impl Into<String>) -> Error {
    Error::MalformedManagedSection {
        filename: None,
        reason: reason.into(),
    }
}

/// Validate that a managed section name can be safely rendered in marker lines.
fn validate_section_name(name: &str) -> Result<()> {
    if name.trim().is_empty() {
        return Err(malformed_section("section name cannot be empty"));
    }

    if name.chars().any(char::is_control) {
        return Err(malformed_section(
            "section name cannot contain control characters",
        ));
    }

    Ok(())
}
