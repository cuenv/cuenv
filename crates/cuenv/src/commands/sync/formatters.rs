//! Formatter execution for post-sync formatting.
//!
//! Runs configured formatters on generated files after sync operations.

use cuenv_core::Result;
use cuenv_core::manifest::{Formatters, NixFormatter, NixFormatterTool, RustFormatter};
use glob::Pattern;
use std::path::Path;
use std::process::Command;
use tracing::{debug, info, warn};

/// Format files using configured formatters.
///
/// Takes a list of file paths that were generated and the formatters config.
/// Matches files against glob patterns and runs the appropriate formatter.
///
/// # Arguments
/// * `files` - List of file paths that were generated (should be absolute paths)
/// * `formatters` - The formatters configuration from the project
/// * `project_root` - Root path of the project (used for pattern matching and running formatters)
/// * `dry_run` - If true, only report what would be formatted
/// * `check` - If true, check formatting without making changes
///
/// # Pattern Matching
/// Patterns are matched against the relative path from `project_root`, allowing
/// patterns like `src/**/*.rs` or `tests/*.go` to work correctly. Invalid glob
/// patterns are logged as warnings and skipped.
///
/// # Returns
/// A string describing what was formatted.
///
/// # Errors
/// Returns an error if:
/// - In check mode and files are not properly formatted
/// - A formatter command fails to execute
pub fn format_generated_files(
    files: &[&Path],
    formatters: &Formatters,
    project_root: &Path,
    dry_run: bool,
    check: bool,
) -> Result<String> {
    let mut output_lines = Vec::new();

    // Collect files by formatter type
    let mut rust_files = Vec::new();
    let mut nix_files = Vec::new();
    let mut go_files = Vec::new();
    let mut cue_files = Vec::new();

    for file in files {
        // Use relative path from project root for pattern matching
        // This allows patterns like "src/**/*.rs" to work correctly
        let relative_path = file.strip_prefix(project_root).unwrap_or(file);
        let path_str = relative_path.to_string_lossy();

        // Check Rust formatter
        if let Some(ref rust) = formatters.rust
            && rust.enabled
            && matches_any_pattern(&path_str, &rust.includes)
        {
            rust_files.push(*file);
        }

        // Check Nix formatter
        if let Some(ref nix) = formatters.nix
            && nix.enabled
            && matches_any_pattern(&path_str, &nix.includes)
        {
            nix_files.push(*file);
        }

        // Check Go formatter
        if let Some(ref go) = formatters.go
            && go.enabled
            && matches_any_pattern(&path_str, &go.includes)
        {
            go_files.push(*file);
        }

        // Check CUE formatter
        if let Some(ref cue) = formatters.cue
            && cue.enabled
            && matches_any_pattern(&path_str, &cue.includes)
        {
            cue_files.push(*file);
        }
    }

    // Run formatters and collect results
    // In check mode, we want to report all failures, not just the first
    let mut errors = Vec::new();

    if !rust_files.is_empty() {
        match run_rust_formatter(
            &rust_files,
            formatters.rust.as_ref(),
            project_root,
            dry_run,
            check,
        ) {
            Ok(result) => output_lines.push(result),
            Err(e) => errors.push(e),
        }
    }

    if !nix_files.is_empty() {
        match run_nix_formatter(
            &nix_files,
            formatters.nix.as_ref(),
            project_root,
            dry_run,
            check,
        ) {
            Ok(result) => output_lines.push(result),
            Err(e) => errors.push(e),
        }
    }

    if !go_files.is_empty() {
        match run_go_formatter(&go_files, project_root, dry_run, check) {
            Ok(result) => output_lines.push(result),
            Err(e) => errors.push(e),
        }
    }

    if !cue_files.is_empty() {
        match run_cue_formatter(&cue_files, project_root, dry_run, check) {
            Ok(result) => output_lines.push(result),
            Err(e) => errors.push(e),
        }
    }

    // Return the first error if any occurred
    if let Some(first_error) = errors.into_iter().next() {
        return Err(first_error);
    }

    if output_lines.is_empty() {
        Ok(String::new())
    } else {
        Ok(output_lines.join("\n"))
    }
}

/// Check if a path matches any of the glob patterns.
///
/// Patterns are matched against the relative path from project root,
/// allowing patterns like `src/**/*.rs` or `tests/*.go` to work correctly.
fn matches_any_pattern(path: &str, patterns: &[String]) -> bool {
    for pattern_str in patterns {
        match Pattern::new(pattern_str) {
            Ok(pattern) => {
                if pattern.matches(path) {
                    return true;
                }
            }
            Err(e) => {
                warn!(
                    pattern = %pattern_str,
                    error = %e,
                    "Invalid glob pattern in formatter configuration; skipping"
                );
            }
        }
    }
    false
}

/// Run rustfmt on files.
///
/// # Errors
///
/// Returns an error if:
/// - In check mode and files are not properly formatted
/// - The formatter command fails to execute
fn run_rust_formatter(
    files: &[&Path],
    config: Option<&RustFormatter>,
    project_root: &Path,
    dry_run: bool,
    check: bool,
) -> Result<String> {
    if dry_run {
        return Ok(format!(
            "Would format {} Rust file(s) with rustfmt",
            files.len()
        ));
    }

    let mut cmd = Command::new("rustfmt");

    if check {
        cmd.arg("--check");
    }

    // Add edition if configured
    if let Some(cfg) = config
        && let Some(ref edition) = cfg.edition
    {
        cmd.arg("--edition").arg(edition);
    }

    // Add file paths
    for file in files {
        cmd.arg(file);
    }

    cmd.current_dir(project_root);

    debug!(?cmd, "Running rustfmt");

    match cmd.output() {
        Ok(output) => {
            if output.status.success() {
                info!(count = files.len(), "Formatted Rust files");
                Ok(format!("Formatted {} Rust file(s)", files.len()))
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                warn!(%stderr, "rustfmt reported issues");
                if check {
                    Err(cuenv_core::Error::configuration(format!(
                        "Rust formatting check failed: {stderr}"
                    )))
                } else {
                    Ok(format!(
                        "Formatted {} Rust file(s) (with warnings)",
                        files.len()
                    ))
                }
            }
        }
        Err(e) => {
            warn!(%e, "Failed to run rustfmt");
            Err(cuenv_core::Error::configuration(format!(
                "Failed to run rustfmt: {e}"
            )))
        }
    }
}

/// Run Nix formatter (nixfmt or alejandra) on files.
///
/// # Errors
///
/// Returns an error if:
/// - In check mode and files are not properly formatted
/// - The formatter command fails to execute
fn run_nix_formatter(
    files: &[&Path],
    config: Option<&NixFormatter>,
    project_root: &Path,
    dry_run: bool,
    check: bool,
) -> Result<String> {
    let tool = config.map_or(NixFormatterTool::default(), |c| c.tool);
    let tool_name = tool.command();

    if dry_run {
        return Ok(format!(
            "Would format {} Nix file(s) with {tool_name}",
            files.len()
        ));
    }

    let mut cmd = Command::new(tool_name);

    if check {
        cmd.arg(tool.check_flag());
    }

    // Add file paths
    for file in files {
        cmd.arg(file);
    }

    cmd.current_dir(project_root);

    debug!(?cmd, "Running Nix formatter");

    match cmd.output() {
        Ok(output) => {
            if output.status.success() {
                info!(count = files.len(), tool_name, "Formatted Nix files");
                Ok(format!(
                    "Formatted {} Nix file(s) with {tool_name}",
                    files.len()
                ))
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                warn!(%stderr, tool_name, "Nix formatter reported issues");
                if check {
                    Err(cuenv_core::Error::configuration(format!(
                        "Nix formatting check failed: {stderr}"
                    )))
                } else {
                    Ok(format!(
                        "Formatted {} Nix file(s) with {tool_name} (with warnings)",
                        files.len()
                    ))
                }
            }
        }
        Err(e) => {
            warn!(%e, tool_name, "Failed to run Nix formatter");
            Err(cuenv_core::Error::configuration(format!(
                "Failed to run {tool_name}: {e}"
            )))
        }
    }
}

/// Run gofmt on files.
///
/// # Errors
///
/// Returns an error if:
/// - In check mode and files are not properly formatted
/// - The formatter command fails to execute
fn run_go_formatter(
    files: &[&Path],
    project_root: &Path,
    dry_run: bool,
    check: bool,
) -> Result<String> {
    if dry_run {
        return Ok(format!(
            "Would format {} Go file(s) with gofmt",
            files.len()
        ));
    }

    let mut cmd = Command::new("gofmt");

    if check {
        cmd.arg("-l"); // List files that need formatting
    } else {
        cmd.arg("-w"); // Write result to (source) file
    }

    // Add file paths
    for file in files {
        cmd.arg(file);
    }

    cmd.current_dir(project_root);

    debug!(?cmd, "Running gofmt");

    match cmd.output() {
        Ok(output) => {
            if output.status.success() {
                if check {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    if stdout.trim().is_empty() {
                        Ok(format!(
                            "Go formatting check passed for {} file(s)",
                            files.len()
                        ))
                    } else {
                        Err(cuenv_core::Error::configuration(format!(
                            "Go formatting check failed - files need formatting:\n{stdout}"
                        )))
                    }
                } else {
                    info!(count = files.len(), "Formatted Go files");
                    Ok(format!("Formatted {} Go file(s)", files.len()))
                }
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                warn!(%stderr, "gofmt reported issues");
                Err(cuenv_core::Error::configuration(format!(
                    "gofmt failed: {stderr}"
                )))
            }
        }
        Err(e) => {
            warn!(%e, "Failed to run gofmt");
            Err(cuenv_core::Error::configuration(format!(
                "Failed to run gofmt: {e}"
            )))
        }
    }
}

/// Run cue fmt on files.
///
/// # Errors
///
/// Returns an error if:
/// - In check mode and files are not properly formatted
/// - The formatter command fails to execute
///
/// # Check Mode
///
/// Uses `cue fmt -d` to display diffs. If stdout is empty, formatting passes.
fn run_cue_formatter(
    files: &[&Path],
    project_root: &Path,
    dry_run: bool,
    check: bool,
) -> Result<String> {
    if dry_run {
        return Ok(format!(
            "Would format {} CUE file(s) with cue fmt",
            files.len()
        ));
    }

    let mut cmd = Command::new("cue");
    cmd.arg("fmt");

    // Use -d flag to show diffs (check mode) or format in-place
    if check {
        cmd.arg("-d");
    }

    // Add file paths
    for file in files {
        cmd.arg(file);
    }

    cmd.current_dir(project_root);

    debug!(?cmd, "Running cue fmt");

    match cmd.output() {
        Ok(output) => {
            if output.status.success() {
                if check {
                    // In check mode with -d, if stdout is empty, files are properly formatted
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    if stdout.trim().is_empty() {
                        Ok(format!(
                            "CUE formatting check passed for {} file(s)",
                            files.len()
                        ))
                    } else {
                        Err(cuenv_core::Error::configuration(format!(
                            "CUE formatting check failed - files need formatting:\n{stdout}"
                        )))
                    }
                } else {
                    info!(count = files.len(), "Formatted CUE files");
                    Ok(format!("Formatted {} CUE file(s)", files.len()))
                }
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                warn!(%stderr, "cue fmt reported issues");
                Err(cuenv_core::Error::configuration(format!(
                    "cue fmt failed: {stderr}"
                )))
            }
        }
        Err(e) => {
            warn!(%e, "Failed to run cue fmt");
            Err(cuenv_core::Error::configuration(format!(
                "Failed to run cue fmt: {e}"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ============================================================================
    // Pattern Matching Tests
    // ============================================================================

    #[test]
    fn test_matches_any_pattern_simple() {
        assert!(matches_any_pattern("foo.rs", &["*.rs".to_string()]));
        assert!(matches_any_pattern("bar.nix", &["*.nix".to_string()]));
        assert!(!matches_any_pattern("foo.rs", &["*.go".to_string()]));
        assert!(matches_any_pattern(
            "foo.rs",
            &["*.go".to_string(), "*.rs".to_string()]
        ));
    }

    #[test]
    fn test_matches_any_pattern_with_directory() {
        // Relative paths should match directory patterns
        assert!(matches_any_pattern("src/lib.rs", &["src/*.rs".to_string()]));
        assert!(matches_any_pattern(
            "src/utils/helpers.rs",
            &["src/**/*.rs".to_string()]
        ));
        assert!(!matches_any_pattern(
            "tests/test.rs",
            &["src/**/*.rs".to_string()]
        ));
    }

    #[test]
    fn test_matches_any_pattern_nested_directories() {
        // Deep nested paths
        assert!(matches_any_pattern(
            "crates/core/src/lib.rs",
            &["**/*.rs".to_string()]
        ));
        assert!(matches_any_pattern(
            "packages/ui/components/Button.tsx",
            &["packages/**/*.tsx".to_string()]
        ));
    }

    #[test]
    fn test_matches_any_pattern_empty_patterns() {
        assert!(!matches_any_pattern("foo.rs", &[]));
    }

    #[test]
    fn test_matches_any_pattern_invalid_pattern_skipped() {
        // Invalid pattern should be skipped, valid one should still match
        assert!(matches_any_pattern(
            "foo.rs",
            &["[invalid".to_string(), "*.rs".to_string()]
        ));
        // Only invalid pattern - should not match
        assert!(!matches_any_pattern("foo.rs", &["[invalid".to_string()]));
    }

    #[test]
    fn test_matches_any_pattern_case_sensitive() {
        // Glob patterns are case-sensitive by default
        assert!(matches_any_pattern("Makefile", &["Makefile".to_string()]));
        assert!(!matches_any_pattern("makefile", &["Makefile".to_string()]));
    }

    // ============================================================================
    // NixFormatterTool Tests
    // ============================================================================

    #[test]
    fn test_nix_formatter_tool_command() {
        assert_eq!(NixFormatterTool::Nixfmt.command(), "nixfmt");
        assert_eq!(NixFormatterTool::Alejandra.command(), "alejandra");
    }

    #[test]
    fn test_nix_formatter_tool_check_flag() {
        assert_eq!(NixFormatterTool::Nixfmt.check_flag(), "--check");
        assert_eq!(NixFormatterTool::Alejandra.check_flag(), "-c");
    }

    #[test]
    fn test_nix_formatter_tool_default() {
        assert_eq!(NixFormatterTool::default(), NixFormatterTool::Nixfmt);
    }

    // ============================================================================
    // File Classification Tests
    // ============================================================================

    #[test]
    fn test_file_classification_rust() {
        let formatters = Formatters {
            rust: Some(RustFormatter {
                enabled: true,
                includes: vec!["*.rs".to_string()],
                edition: None,
            }),
            ..Default::default()
        };

        // Would need to test with actual file classification
        // This validates the patterns are set up correctly
        assert!(formatters.rust.as_ref().unwrap().enabled);
        assert_eq!(formatters.rust.as_ref().unwrap().includes, vec!["*.rs"]);
    }

    #[test]
    fn test_file_classification_disabled_formatter() {
        let formatters = Formatters {
            rust: Some(RustFormatter {
                enabled: false,
                includes: vec!["*.rs".to_string()],
                edition: None,
            }),
            ..Default::default()
        };

        assert!(!formatters.rust.as_ref().unwrap().enabled);
    }

    #[test]
    fn test_file_classification_with_directory_patterns() {
        let formatters = Formatters {
            rust: Some(RustFormatter {
                enabled: true,
                includes: vec!["src/**/*.rs".to_string(), "tests/**/*.rs".to_string()],
                edition: Some("2024".to_string()),
            }),
            ..Default::default()
        };

        let patterns = &formatters.rust.as_ref().unwrap().includes;

        // Files that should match
        assert!(matches_any_pattern("src/lib.rs", patterns));
        assert!(matches_any_pattern("src/utils/mod.rs", patterns));
        assert!(matches_any_pattern("tests/integration.rs", patterns));

        // Files that should not match
        assert!(!matches_any_pattern("build/output.rs", patterns));
        assert!(!matches_any_pattern("lib.rs", patterns)); // Not in src or tests
    }
}
