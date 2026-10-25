//! Formatter execution shared by `cuenv fmt` and post-sync formatting.
//!
//! Formatters run inside the project's resolved environment, the same one
//! `cuenv exec` builds, so `cuenv fmt` uses the toolchain the project
//! declares rather than whatever happens to be on cuenv's own `PATH`.

use crate::commands::CommandExecutor;
use crate::commands::exec::{ProjectEnvironment, ProjectEnvironmentRequest};
use cuenv_core::DryRun;
use cuenv_core::Result;
use cuenv_core::environment::Environment;
use cuenv_core::manifest::{Formatters, NixFormatter, NixFormatterTool, RustFormatter};
use glob::Pattern;
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, Output};
use tracing::{debug, info, warn};

/// Command name of the Rust formatter.
pub const RUSTFMT: &str = "rustfmt";
/// Command name of the Go formatter.
pub const GOFMT: &str = "gofmt";
/// Command name of the CUE formatter.
pub const CUE: &str = "cue";

/// Whether formatters only check files or rewrite them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormatMode {
    /// Report files that need formatting without changing them.
    Check,
    /// Rewrite files in place.
    Fix,
}

impl FormatMode {
    /// Returns `true` in check mode.
    #[must_use]
    pub const fn is_check(self) -> bool {
        matches!(self, Self::Check)
    }

    /// The verb describing this mode, for log messages.
    #[must_use]
    pub const fn verb(self) -> &'static str {
        match self {
            Self::Check => "check",
            Self::Fix => "format",
        }
    }
}

/// Environments that formatter commands run in, keyed by command name.
///
/// A command without a resolved environment runs in cuenv's inherited
/// environment.
#[derive(Debug, Default)]
pub struct FormatterEnvironments {
    by_command: HashMap<String, Environment>,
    inherited: Environment,
}

impl FormatterEnvironments {
    /// Run every formatter in cuenv's inherited environment.
    #[must_use]
    pub fn inherited() -> Self {
        Self::default()
    }

    /// Use `environment` for `command`.
    pub fn insert(&mut self, command: impl Into<String>, environment: Environment) {
        self.by_command.insert(command.into(), environment);
    }

    /// The environment `command` runs in.
    #[must_use]
    pub fn for_command(&self, command: &str) -> &Environment {
        self.by_command.get(command).unwrap_or(&self.inherited)
    }

    /// Resolve the project environment for each formatter command.
    ///
    /// Reuses the `cuenv exec` environment: the project's runtime and tools
    /// are loaded once, and each command gets the CUE `env` variables its
    /// `allowExec` policies permit. When the project configures no runtime
    /// or tools, formatters resolve against the inherited `PATH`.
    ///
    /// # Errors
    ///
    /// Returns an error if the project environment cannot be resolved.
    pub async fn resolve(
        request: ProjectEnvironmentRequest<'_>,
        commands: &[&str],
        executor: &CommandExecutor,
    ) -> Result<Self> {
        let mut environments = Self::inherited();
        if commands.is_empty() {
            return Ok(environments);
        }

        let project_env = ProjectEnvironment::load(request, executor).await?;
        for command in commands {
            let command_env = project_env.for_command(command).await?;
            environments.insert(*command, command_env.environment().clone());
        }
        Ok(environments)
    }
}

/// Where and how formatters run.
#[derive(Clone, Copy, Debug)]
pub struct FormatterContext<'a> {
    /// Project root; formatters run with this working directory.
    pub project_root: &'a Path,
    /// When set, only report what would be formatted.
    pub dry_run: DryRun,
    /// Check or fix.
    pub mode: FormatMode,
    /// Environments formatter commands run in.
    pub environments: &'a FormatterEnvironments,
}

/// Command names of the enabled formatters.
#[must_use]
pub fn enabled_formatter_commands(formatters: &Formatters) -> Vec<&'static str> {
    let mut commands = Vec::new();
    if formatters.rust.as_ref().is_some_and(|rust| rust.enabled) {
        commands.push(RUSTFMT);
    }
    if let Some(nix) = formatters.nix.as_ref().filter(|nix| nix.enabled) {
        commands.push(nix.tool.command());
    }
    if formatters.go.as_ref().is_some_and(|go| go.enabled) {
        commands.push(GOFMT);
    }
    if formatters.cue.as_ref().is_some_and(|cue| cue.enabled) {
        commands.push(CUE);
    }
    commands
}

/// Format files using configured formatters.
///
/// Takes a list of file paths that were generated and the formatters config.
/// Matches files against glob patterns and runs the appropriate formatter.
///
/// # Arguments
/// * `files` - List of file paths that were generated (should be absolute paths)
/// * `formatters` - The formatters configuration from the project
/// * `context` - Project root, dry-run, mode, and formatter environments
///
/// # Pattern Matching
/// Patterns are matched against the relative path from the project root,
/// allowing patterns like `src/**/*.rs` or `tests/*.go` to work correctly.
/// Invalid glob patterns are logged as warnings and skipped.
///
/// # Returns
/// A string describing what was formatted.
///
/// # Errors
/// Returns an error if:
/// - In check mode and files are not properly formatted
/// - A formatter command fails to execute or exits non-zero
pub fn format_generated_files(
    files: &[&Path],
    formatters: &Formatters,
    context: &FormatterContext<'_>,
) -> Result<String> {
    let project_root = context.project_root;
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
        match run_rust_formatter(&rust_files, formatters.rust.as_ref(), context) {
            Ok(result) => output_lines.push(result),
            Err(e) => errors.push(e),
        }
    }

    if !nix_files.is_empty() {
        match run_nix_formatter(&nix_files, formatters.nix.as_ref(), context) {
            Ok(result) => output_lines.push(result),
            Err(e) => errors.push(e),
        }
    }

    if !go_files.is_empty() {
        match run_go_formatter(&go_files, context) {
            Ok(result) => output_lines.push(result),
            Err(e) => errors.push(e),
        }
    }

    if !cue_files.is_empty() {
        match run_cue_formatter(&cue_files, context) {
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
pub fn matches_any_pattern(path: &str, patterns: &[String]) -> bool {
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

/// One formatter process invocation.
struct FormatterInvocation<'a> {
    /// Command name, resolved against the formatter environment's `PATH`.
    program: &'a str,
    /// Arguments placed before the file paths.
    args: Vec<OsString>,
    /// Files to format.
    files: &'a [&'a Path],
}

/// Run a formatter in its environment and return its output.
///
/// The program is resolved against the environment's `PATH` (falling back to
/// the inherited `PATH`), and the process inherits cuenv's environment
/// overlaid with the formatter environment.
///
/// # Errors
///
/// Returns an error if the process cannot be started.
fn run_formatter_process(
    invocation: &FormatterInvocation<'_>,
    context: &FormatterContext<'_>,
) -> Result<Output> {
    let environment = context.environments.for_command(invocation.program);
    let resolved = environment.resolve_command(invocation.program);

    let mut cmd = Command::new(&resolved);
    cmd.args(&invocation.args)
        .args(invocation.files)
        .envs(environment.iter())
        .current_dir(context.project_root);

    debug!(program = invocation.program, %resolved, ?cmd, "Running formatter");

    cmd.output().map_err(|e| {
        debug!(%e, program = invocation.program, %resolved, "Failed to run formatter");
        cuenv_core::Error::configuration(format!(
            "Failed to run {} ({resolved}): {e}",
            invocation.program
        ))
    })
}

/// Turn a non-zero formatter exit into an error.
///
/// Formatters exit non-zero in fix mode only when they fail (a parse error,
/// a missing toolchain), so a non-zero exit is an error in every mode.
/// `failure` prefixes the error message.
///
/// # Errors
///
/// Returns an error when `output.status` is not successful.
fn ensure_formatter_succeeded(output: &Output, failure: &str) -> Result<()> {
    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let details = [stderr.trim(), stdout.trim()]
        .into_iter()
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let details = if details.is_empty() {
        output.status.to_string()
    } else {
        details
    };

    debug!(%details, failure, "Formatter failed");
    Err(cuenv_core::Error::configuration(format!(
        "{failure}: {details}"
    )))
}

/// Run rustfmt on files.
///
/// # Errors
///
/// Returns an error if:
/// - In check mode and files are not properly formatted
/// - rustfmt cannot be started or exits non-zero
pub fn run_rust_formatter(
    files: &[&Path],
    config: Option<&RustFormatter>,
    context: &FormatterContext<'_>,
) -> Result<String> {
    if context.dry_run.is_dry_run() {
        return Ok(format!(
            "Would format {} Rust file(s) with rustfmt",
            files.len()
        ));
    }

    let mut args: Vec<OsString> = Vec::new();
    if context.mode.is_check() {
        args.push("--check".into());
    }
    if let Some(edition) = config.and_then(|cfg| cfg.edition.as_ref()) {
        args.push("--edition".into());
        args.push(edition.into());
    }

    let invocation = FormatterInvocation {
        program: RUSTFMT,
        args,
        files,
    };
    let output = run_formatter_process(&invocation, context)?;

    match context.mode {
        FormatMode::Check => {
            ensure_formatter_succeeded(&output, "Rust formatting check failed")?;
            Ok(format!(
                "Rust formatting check passed for {} file(s)",
                files.len()
            ))
        }
        FormatMode::Fix => {
            ensure_formatter_succeeded(&output, "rustfmt failed")?;
            info!(count = files.len(), "Formatted Rust files");
            Ok(format!("Formatted {} Rust file(s)", files.len()))
        }
    }
}

/// Run Nix formatter (nixfmt or alejandra) on files.
///
/// # Errors
///
/// Returns an error if:
/// - In check mode and files are not properly formatted
/// - The formatter cannot be started or exits non-zero
pub fn run_nix_formatter(
    files: &[&Path],
    config: Option<&NixFormatter>,
    context: &FormatterContext<'_>,
) -> Result<String> {
    let tool = config.map_or(NixFormatterTool::default(), |c| c.tool);
    let tool_name = tool.command();

    if context.dry_run.is_dry_run() {
        return Ok(format!(
            "Would format {} Nix file(s) with {tool_name}",
            files.len()
        ));
    }

    let args: Vec<OsString> = if context.mode.is_check() {
        vec![tool.check_flag().into()]
    } else {
        Vec::new()
    };

    let invocation = FormatterInvocation {
        program: tool_name,
        args,
        files,
    };
    let output = run_formatter_process(&invocation, context)?;

    match context.mode {
        FormatMode::Check => {
            ensure_formatter_succeeded(&output, "Nix formatting check failed")?;
            Ok(format!(
                "Nix formatting check passed for {} file(s) with {tool_name}",
                files.len()
            ))
        }
        FormatMode::Fix => {
            ensure_formatter_succeeded(&output, &format!("{tool_name} failed"))?;
            info!(count = files.len(), tool_name, "Formatted Nix files");
            Ok(format!(
                "Formatted {} Nix file(s) with {tool_name}",
                files.len()
            ))
        }
    }
}

/// Run gofmt on files.
///
/// # Errors
///
/// Returns an error if:
/// - In check mode and files are not properly formatted
/// - gofmt cannot be started or exits non-zero
pub fn run_go_formatter(files: &[&Path], context: &FormatterContext<'_>) -> Result<String> {
    if context.dry_run.is_dry_run() {
        return Ok(format!(
            "Would format {} Go file(s) with gofmt",
            files.len()
        ));
    }

    // -l lists files that need formatting; -w writes the result in place.
    let flag = if context.mode.is_check() { "-l" } else { "-w" };
    let invocation = FormatterInvocation {
        program: GOFMT,
        args: vec![flag.into()],
        files,
    };
    let output = run_formatter_process(&invocation, context)?;
    ensure_formatter_succeeded(&output, "gofmt failed")?;

    match context.mode {
        FormatMode::Check => {
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
        }
        FormatMode::Fix => {
            info!(count = files.len(), "Formatted Go files");
            Ok(format!("Formatted {} Go file(s)", files.len()))
        }
    }
}

/// Run cue fmt on files.
///
/// # Errors
///
/// Returns an error if:
/// - In check mode and files are not properly formatted
/// - cue cannot be started or exits non-zero
///
/// # Check Mode
///
/// Uses `cue fmt -d` to display diffs. If stdout is empty, formatting passes.
pub fn run_cue_formatter(files: &[&Path], context: &FormatterContext<'_>) -> Result<String> {
    if context.dry_run.is_dry_run() {
        return Ok(format!(
            "Would format {} CUE file(s) with cue fmt",
            files.len()
        ));
    }

    // -d shows diffs (check mode); without it files are formatted in place.
    let mut args: Vec<OsString> = vec!["fmt".into()];
    if context.mode.is_check() {
        args.push("-d".into());
    }
    let invocation = FormatterInvocation {
        program: CUE,
        args,
        files,
    };
    let output = run_formatter_process(&invocation, context)?;
    ensure_formatter_succeeded(&output, "cue fmt failed")?;

    match context.mode {
        FormatMode::Check => {
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
        }
        FormatMode::Fix => {
            info!(count = files.len(), "Formatted CUE files");
            Ok(format!("Formatted {} CUE file(s)", files.len()))
        }
    }
}

/// Test helpers shared by formatter and `cuenv fmt` tests.
#[cfg(test)]
pub(crate) mod test_support {
    use std::path::Path;
    use std::process::Command;

    /// Write an executable shell script at `path`.
    ///
    /// The file is written by a child `sh`, so this process never holds a
    /// writable descriptor to it. Writing it directly would let a concurrently
    /// forked test process inherit that descriptor, and executing the script
    /// would then fail with `ETXTBSY` ("Text file busy").
    #[cfg(unix)]
    pub fn install_test_script(path: &Path, body: &str) {
        let status = Command::new("/bin/sh")
            .arg("-c")
            .arg("printf '%s\\n' \"$1\" > \"$2\" && chmod 755 \"$2\"")
            .arg("sh")
            .arg(format!("#!/bin/sh\n{body}"))
            .arg(path)
            .status()
            .unwrap();
        assert!(status.success(), "failed to install {}", path.display());
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

    // ============================================================================
    // Mode, Environment, and Exit Status Tests
    // ============================================================================

    #[test]
    fn test_format_mode_is_check() {
        assert!(FormatMode::Check.is_check());
        assert!(!FormatMode::Fix.is_check());
        assert_eq!(FormatMode::Check.verb(), "check");
        assert_eq!(FormatMode::Fix.verb(), "format");
    }

    #[test]
    fn test_formatter_environments_fall_back_to_inherited() {
        let mut environments = FormatterEnvironments::inherited();
        assert!(environments.for_command(RUSTFMT).is_empty());

        let mut project = Environment::new();
        project.set("PATH".to_string(), "/project/bin".to_string());
        environments.insert(RUSTFMT, project);

        assert_eq!(
            environments.for_command(RUSTFMT).get("PATH"),
            Some("/project/bin")
        );
        assert!(environments.for_command(GOFMT).is_empty());
    }

    #[test]
    fn test_enabled_formatter_commands() {
        let formatters = Formatters {
            rust: Some(RustFormatter {
                enabled: true,
                includes: vec!["*.rs".to_string()],
                edition: None,
            }),
            nix: Some(NixFormatter {
                enabled: true,
                includes: vec!["*.nix".to_string()],
                tool: NixFormatterTool::Alejandra,
            }),
            ..Default::default()
        };
        assert_eq!(
            enabled_formatter_commands(&formatters),
            ["rustfmt", "alejandra"]
        );
        assert!(enabled_formatter_commands(&Formatters::default()).is_empty());
    }

    #[cfg(unix)]
    mod process {
        use super::*;
        use std::path::PathBuf;
        use tempfile::TempDir;

        /// A project directory with a source file and a `bin` directory of
        /// fake formatter executables.
        struct FakeProject {
            dir: TempDir,
        }

        impl FakeProject {
            fn new() -> Self {
                let dir = TempDir::new().unwrap();
                std::fs::create_dir(dir.path().join("bin")).unwrap();
                std::fs::write(dir.path().join("lib.rs"), "fn main() {}\n").unwrap();
                std::fs::write(dir.path().join("flake.nix"), "{ }\n").unwrap();
                Self { dir }
            }

            fn root(&self) -> &Path {
                self.dir.path()
            }

            fn bin(&self) -> PathBuf {
                self.dir.path().join("bin")
            }

            fn marker(&self) -> PathBuf {
                self.dir.path().join("invoked")
            }

            /// Install an executable `name` that runs `body` as a shell script.
            fn install(&self, name: &str, body: &str) {
                super::super::test_support::install_test_script(&self.bin().join(name), body);
            }

            /// Environments resolving every formatter against the fake `bin`.
            fn environments(&self) -> FormatterEnvironments {
                let mut environment = Environment::new();
                environment.set(
                    "PATH".to_string(),
                    self.bin().to_string_lossy().into_owned(),
                );
                environment.set(
                    "FAKE_FORMATTER_MARKER".to_string(),
                    self.marker().to_string_lossy().into_owned(),
                );
                let mut environments = FormatterEnvironments::inherited();
                for command in [RUSTFMT, "nixfmt", GOFMT, CUE] {
                    environments.insert(command, environment.clone());
                }
                environments
            }
        }

        fn context<'a>(
            project: &'a FakeProject,
            mode: FormatMode,
            environments: &'a FormatterEnvironments,
        ) -> FormatterContext<'a> {
            FormatterContext {
                project_root: project.root(),
                dry_run: DryRun::No,
                mode,
                environments,
            }
        }

        const FAILING_RUSTFMT: &str =
            "echo 'error: rustup could not choose a version of rustfmt to run' >&2\nexit 1";

        #[test]
        fn rust_formatter_fix_mode_nonzero_exit_is_error() {
            let project = FakeProject::new();
            project.install(RUSTFMT, FAILING_RUSTFMT);
            let environments = project.environments();
            let file = project.root().join("lib.rs");

            let result = run_rust_formatter(
                &[file.as_path()],
                None,
                &context(&project, FormatMode::Fix, &environments),
            );

            let error = result.expect_err("non-zero rustfmt exit must fail in fix mode");
            let message = error.to_string();
            assert!(message.contains("rustfmt failed"), "{message}");
            assert!(message.contains("rustup could not choose"), "{message}");
            assert!(!message.contains("with warnings"), "{message}");
        }

        #[test]
        fn rust_formatter_check_mode_nonzero_exit_is_error() {
            let project = FakeProject::new();
            project.install(RUSTFMT, "echo 'Diff in lib.rs'\nexit 1");
            let environments = project.environments();
            let file = project.root().join("lib.rs");

            let error = run_rust_formatter(
                &[file.as_path()],
                None,
                &context(&project, FormatMode::Check, &environments),
            )
            .expect_err("non-zero rustfmt --check exit must fail");
            let message = error.to_string();
            assert!(
                message.contains("Rust formatting check failed"),
                "{message}"
            );
            assert!(message.contains("Diff in lib.rs"), "{message}");
        }

        #[test]
        fn nix_formatter_fix_mode_nonzero_exit_is_error() {
            let project = FakeProject::new();
            project.install("nixfmt", "echo 'syntax error' >&2\nexit 1");
            let environments = project.environments();
            let file = project.root().join("flake.nix");

            let error = run_nix_formatter(
                &[file.as_path()],
                None,
                &context(&project, FormatMode::Fix, &environments),
            )
            .expect_err("non-zero nixfmt exit must fail in fix mode");
            let message = error.to_string();
            assert!(message.contains("nixfmt failed"), "{message}");
            assert!(message.contains("syntax error"), "{message}");
        }

        #[test]
        fn format_generated_files_fix_mode_propagates_formatter_failure() {
            let project = FakeProject::new();
            project.install(RUSTFMT, FAILING_RUSTFMT);
            let environments = project.environments();
            let file = project.root().join("lib.rs");
            let formatters = Formatters {
                rust: Some(RustFormatter {
                    enabled: true,
                    includes: vec!["*.rs".to_string()],
                    edition: None,
                }),
                ..Default::default()
            };

            let result = format_generated_files(
                &[file.as_path()],
                &formatters,
                &context(&project, FormatMode::Fix, &environments),
            );

            assert!(result.is_err(), "{result:?}");
        }

        #[test]
        fn rust_formatter_resolves_against_project_environment_path() {
            let project = FakeProject::new();
            // Record the arguments in the marker file named by a project
            // environment variable, proving both PATH resolution and env
            // propagation.
            project.install(RUSTFMT, "echo \"$@\" > \"$FAKE_FORMATTER_MARKER\"");
            let environments = project.environments();
            let file = project.root().join("lib.rs");
            let config = RustFormatter {
                enabled: true,
                includes: vec!["*.rs".to_string()],
                edition: Some("2024".to_string()),
            };

            let output = run_rust_formatter(
                &[file.as_path()],
                Some(&config),
                &context(&project, FormatMode::Fix, &environments),
            )
            .unwrap();

            assert_eq!(output, "Formatted 1 Rust file(s)");
            let recorded = std::fs::read_to_string(project.marker()).unwrap();
            assert!(recorded.contains("--edition 2024"), "{recorded}");
            assert!(recorded.contains("lib.rs"), "{recorded}");
            assert!(!recorded.contains("--check"), "{recorded}");
        }

        #[tokio::test]
        async fn resolve_without_cue_module_uses_inherited_path() {
            // A directory with no CUE module and no lockfile: formatters must
            // fall back to the inherited environment rather than erroring.
            let project = FakeProject::new();
            let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
            let executor = CommandExecutor::new(sender, "cuenv".to_string());
            let path = project.root().to_string_lossy().into_owned();

            let environments = FormatterEnvironments::resolve(
                ProjectEnvironmentRequest {
                    path: &path,
                    package: "cuenv",
                    environment_override: None,
                },
                &[RUSTFMT],
                &executor,
            )
            .await
            .unwrap();

            assert_eq!(
                environments.for_command(RUSTFMT).get("PATH"),
                std::env::var("PATH").ok().as_deref()
            );
        }
    }
}
