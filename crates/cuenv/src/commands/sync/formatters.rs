//! Formatter execution for post-sync formatting.
//!
//! Runs configured formatters on generated files after sync operations.

use cuenv_core::manifest::{Formatters, NixFormatter, RustFormatter};
use cuenv_core::Result;
use glob::Pattern;
use std::path::Path;
use std::process::Command;
use tracing::{debug, info, warn};

/// Format files using configured formatters.
///
/// Takes a list of file paths that were generated and the formatters config.
/// Matches files against patterns and runs the appropriate formatter.
///
/// # Arguments
/// * `files` - List of file paths that were generated
/// * `formatters` - The formatters configuration from the project
/// * `project_root` - Root path of the project (for running formatters)
/// * `dry_run` - If true, only report what would be formatted
/// * `check` - If true, check formatting without making changes
///
/// # Returns
/// A string describing what was formatted, or errors encountered.
///
/// # Errors
/// Returns an error if pattern matching fails during file classification.
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
        let file_name = file
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();

        // Check Rust formatter
        if let Some(ref rust) = formatters.rust
            && rust.enabled && matches_any_pattern(file_name, &rust.includes) {
                rust_files.push(*file);
            }

        // Check Nix formatter
        if let Some(ref nix) = formatters.nix
            && nix.enabled && matches_any_pattern(file_name, &nix.includes) {
                nix_files.push(*file);
            }

        // Check Go formatter
        if let Some(ref go) = formatters.go
            && go.enabled && matches_any_pattern(file_name, &go.includes) {
                go_files.push(*file);
            }

        // Check CUE formatter
        if let Some(ref cue) = formatters.cue
            && cue.enabled && matches_any_pattern(file_name, &cue.includes) {
                cue_files.push(*file);
            }
    }

    // Run formatters
    if !rust_files.is_empty() {
        let result =
            run_rust_formatter(&rust_files, formatters.rust.as_ref(), project_root, dry_run, check);
        output_lines.push(result);
    }

    if !nix_files.is_empty() {
        let result =
            run_nix_formatter(&nix_files, formatters.nix.as_ref(), project_root, dry_run, check);
        output_lines.push(result);
    }

    if !go_files.is_empty() {
        let result = run_go_formatter(&go_files, project_root, dry_run, check);
        output_lines.push(result);
    }

    if !cue_files.is_empty() {
        let result = run_cue_formatter(&cue_files, project_root, dry_run, check);
        output_lines.push(result);
    }

    if output_lines.is_empty() {
        Ok(String::new())
    } else {
        Ok(output_lines.join("\n"))
    }
}

/// Check if a filename matches any of the glob patterns.
fn matches_any_pattern(filename: &str, patterns: &[String]) -> bool {
    for pattern_str in patterns {
        if let Ok(pattern) = Pattern::new(pattern_str)
            && pattern.matches(filename) {
                return true;
            }
    }
    false
}

/// Run rustfmt on files.
fn run_rust_formatter(
    files: &[&Path],
    config: Option<&RustFormatter>,
    project_root: &Path,
    dry_run: bool,
    check: bool,
) -> String {
    if dry_run {
        return format!("Would format {} Rust file(s) with rustfmt", files.len());
    }

    let mut cmd = Command::new("rustfmt");

    if check {
        cmd.arg("--check");
    }

    // Add edition if configured
    if let Some(cfg) = config
        && let Some(ref edition) = cfg.edition {
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
                format!("Formatted {} Rust file(s)", files.len())
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                warn!(%stderr, "rustfmt reported issues");
                if check {
                    format!("Rust formatting check failed: {stderr}")
                } else {
                    format!("Formatted {} Rust file(s) (with warnings)", files.len())
                }
            }
        }
        Err(e) => {
            warn!(%e, "Failed to run rustfmt");
            format!("Failed to run rustfmt: {e}")
        }
    }
}

/// Run Nix formatter (nixfmt or alejandra) on files.
fn run_nix_formatter(
    files: &[&Path],
    config: Option<&NixFormatter>,
    project_root: &Path,
    dry_run: bool,
    check: bool,
) -> String {
    let tool = config.map_or("nixfmt", |c| c.tool.as_str());

    if dry_run {
        return format!("Would format {} Nix file(s) with {tool}", files.len());
    }

    let mut cmd = Command::new(tool);

    if check {
        match tool {
            "alejandra" => {
                cmd.arg("-c");
            }
            _ => {
                cmd.arg("--check");
            }
        }
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
                info!(count = files.len(), tool, "Formatted Nix files");
                format!("Formatted {} Nix file(s) with {tool}", files.len())
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                warn!(%stderr, tool, "Nix formatter reported issues");
                if check {
                    format!("Nix formatting check failed: {stderr}")
                } else {
                    format!(
                        "Formatted {} Nix file(s) with {tool} (with warnings)",
                        files.len()
                    )
                }
            }
        }
        Err(e) => {
            warn!(%e, tool, "Failed to run Nix formatter");
            format!("Failed to run {tool}: {e}")
        }
    }
}

/// Run gofmt on files.
fn run_go_formatter(files: &[&Path], project_root: &Path, dry_run: bool, check: bool) -> String {
    if dry_run {
        return format!("Would format {} Go file(s) with gofmt", files.len());
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
                        format!("Go formatting check passed for {} file(s)", files.len())
                    } else {
                        format!("Go formatting check failed - files need formatting:\n{stdout}")
                    }
                } else {
                    info!(count = files.len(), "Formatted Go files");
                    format!("Formatted {} Go file(s)", files.len())
                }
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                warn!(%stderr, "gofmt reported issues");
                format!("gofmt failed: {stderr}")
            }
        }
        Err(e) => {
            warn!(%e, "Failed to run gofmt");
            format!("Failed to run gofmt: {e}")
        }
    }
}

/// Run cue fmt on files.
fn run_cue_formatter(files: &[&Path], project_root: &Path, dry_run: bool, check: bool) -> String {
    if dry_run {
        return format!("Would format {} CUE file(s) with cue fmt", files.len());
    }

    // CUE fmt doesn't have a --check flag, so we format in-place
    // For check mode, we'd need to compare before/after, which is complex
    if check {
        // For now, just report that we can't do check mode for CUE
        return format!(
            "CUE formatter check mode not supported (would format {} file(s))",
            files.len()
        );
    }

    let mut cmd = Command::new("cue");
    cmd.arg("fmt");

    // Add file paths
    for file in files {
        cmd.arg(file);
    }

    cmd.current_dir(project_root);

    debug!(?cmd, "Running cue fmt");

    match cmd.output() {
        Ok(output) => {
            if output.status.success() {
                info!(count = files.len(), "Formatted CUE files");
                format!("Formatted {} CUE file(s)", files.len())
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                warn!(%stderr, "cue fmt reported issues");
                format!("cue fmt failed: {stderr}")
            }
        }
        Err(e) => {
            warn!(%e, "Failed to run cue fmt");
            format!("Failed to run cue fmt: {e}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_matches_any_pattern() {
        assert!(matches_any_pattern("foo.rs", &["*.rs".to_string()]));
        assert!(matches_any_pattern("bar.nix", &["*.nix".to_string()]));
        assert!(!matches_any_pattern("foo.rs", &["*.go".to_string()]));
        assert!(matches_any_pattern(
            "foo.rs",
            &["*.go".to_string(), "*.rs".to_string()]
        ));
    }
}
