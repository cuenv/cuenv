//! Format command implementation.
//!
//! Formats all files in a repository based on the formatters configuration
//! defined in `env.cue`. Uses the same formatter runners as the sync command
//! but discovers files by walking the directory tree.
//!
//! Formatters run in the project's resolved environment (the one `cuenv exec`
//! uses), so the project's declared toolchain is used rather than whatever is
//! on cuenv's own `PATH`.

use crate::commands::exec::ProjectEnvironmentRequest;
use crate::commands::sync::formatters::{CUE, GOFMT, RUSTFMT};
use crate::commands::sync::{
    FormatMode, FormatterContext, FormatterEnvironments, matches_any_pattern, run_cue_formatter,
    run_go_formatter, run_nix_formatter, run_rust_formatter,
};
use crate::commands::{CommandExecutor, relative_path_from_root};
use cuenv_core::Result;
use cuenv_core::manifest::{Base, Formatters, NixFormatterTool};
use ignore::WalkBuilder;
use std::path::{Path, PathBuf};
use tracing::{debug, info};

/// Files discovered for each formatter type.
struct DiscoveredFiles {
    rust: Vec<PathBuf>,
    nix: Vec<PathBuf>,
    go: Vec<PathBuf>,
    cue: Vec<PathBuf>,
}

impl DiscoveredFiles {
    fn is_empty(&self) -> bool {
        self.rust.is_empty() && self.nix.is_empty() && self.go.is_empty() && self.cue.is_empty()
    }

    fn total_count(&self) -> usize {
        self.rust.len() + self.nix.len() + self.go.len() + self.cue.len()
    }
}

/// Check if a formatter should be included based on --only filter.
fn should_include(formatter_name: &str, only: Option<&[String]>) -> bool {
    match only {
        None => true,
        Some(list) => list.iter().any(|s| s.eq_ignore_ascii_case(formatter_name)),
    }
}

/// Discover all files matching formatter patterns.
///
/// Walks the project directory respecting .gitignore and matches files
/// against the configured formatter patterns.
fn discover_files(
    project_root: &Path,
    formatters: &Formatters,
    only: Option<&[String]>,
) -> DiscoveredFiles {
    let walker = WalkBuilder::new(project_root)
        .follow_links(true)
        .standard_filters(true) // Respects .gitignore
        .build();

    let mut rust_files = Vec::new();
    let mut nix_files = Vec::new();
    let mut go_files = Vec::new();
    let mut cue_files = Vec::new();

    for entry in walker.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let relative = path.strip_prefix(project_root).unwrap_or(path);
        let path_str = relative.to_string_lossy();

        // Check Rust formatter
        if should_include("rust", only)
            && let Some(ref rust) = formatters.rust
            && rust.enabled
            && matches_any_pattern(&path_str, &rust.includes)
        {
            rust_files.push(path.to_path_buf());
        }

        // Check Nix formatter
        if should_include("nix", only)
            && let Some(ref nix) = formatters.nix
            && nix.enabled
            && matches_any_pattern(&path_str, &nix.includes)
        {
            nix_files.push(path.to_path_buf());
        }

        // Check Go formatter
        if should_include("go", only)
            && let Some(ref go) = formatters.go
            && go.enabled
            && matches_any_pattern(&path_str, &go.includes)
        {
            go_files.push(path.to_path_buf());
        }

        // Check CUE formatter
        if should_include("cue", only)
            && let Some(ref cue) = formatters.cue
            && cue.enabled
            && matches_any_pattern(&path_str, &cue.includes)
        {
            cue_files.push(path.to_path_buf());
        }
    }

    debug!(
        rust = rust_files.len(),
        nix = nix_files.len(),
        go = go_files.len(),
        cue = cue_files.len(),
        "Discovered files for formatting"
    );

    DiscoveredFiles {
        rust: rust_files,
        nix: nix_files,
        go: go_files,
        cue: cue_files,
    }
}

/// Load the Base configuration from the CUE module.
///
/// Uses the executor's cached module evaluation, which the project
/// environment resolution reuses.
fn load_base_config(path: &str, executor: &CommandExecutor) -> Result<Base> {
    let target_path = Path::new(path).canonicalize().map_err(|e| {
        cuenv_core::Error::io_with_path("canonicalize path", Path::new(path).to_path_buf(), e)
    })?;

    let module = executor.get_module(&target_path)?;
    let relative_path = relative_path_from_root(&module.root, &target_path);
    let instance = module.get(&relative_path).ok_or_else(|| {
        cuenv_core::Error::configuration(format!(
            "No CUE instance found at path: {} (relative: {})",
            target_path.display(),
            relative_path.display()
        ))
    })?;

    instance.deserialize()
}

/// Request for the format command.
#[derive(Clone, Copy, Debug)]
pub struct FmtRequest<'a> {
    /// Path to the CUE module directory.
    pub path: &'a str,
    /// CUE package name to evaluate.
    pub package: &'a str,
    /// Check (default) or fix.
    pub mode: FormatMode,
    /// Optional list of formatters to run (e.g., `["rust", "go"]`).
    pub only: Option<&'a [String]>,
    /// Optional environment name whose `env` overlay formatters run with.
    pub environment_override: Option<&'a str>,
}

/// Formatter commands that have files to format.
fn commands_to_run(files: &DiscoveredFiles, formatters: &Formatters) -> Vec<&'static str> {
    let nix_tool = formatters
        .nix
        .as_ref()
        .map_or(NixFormatterTool::default(), |nix| nix.tool);
    [
        (!files.rust.is_empty()).then_some(RUSTFMT),
        (!files.nix.is_empty()).then_some(nix_tool.command()),
        (!files.go.is_empty()).then_some(GOFMT),
        (!files.cue.is_empty()).then_some(CUE),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// Execute the format command.
///
/// Formatters run in the project's resolved environment. When the project
/// configures no runtime or tools, they run with the inherited `PATH`.
///
/// # Returns
/// A string describing the formatting results.
///
/// # Errors
/// Returns an error if:
/// - No formatters are configured in env.cue
/// - The project environment cannot be resolved
/// - In check mode and files need formatting
/// - A formatter command fails to execute or exits non-zero
pub async fn execute_fmt(request: FmtRequest<'_>, executor: &CommandExecutor) -> Result<String> {
    let FmtRequest {
        path,
        package,
        mode,
        only,
        environment_override,
    } = request;

    // Load config
    let config = load_base_config(path, executor)?;

    // Check formatters exist
    let formatters = config.formatters.ok_or_else(|| {
        cuenv_core::Error::configuration(
            "No formatters configured in env.cue\n\n\
             Add a formatters block to your configuration:\n\n\
               formatters: {\n\
                   rust: {}\n\
                   nix: { tool: \"nixfmt\" }\n\
               }",
        )
    })?;

    // Discover files
    let project_root = Path::new(path).canonicalize().map_err(|e| {
        cuenv_core::Error::io_with_path("canonicalize path", Path::new(path).to_path_buf(), e)
    })?;

    let files = discover_files(&project_root, &formatters, only);

    if files.is_empty() {
        return Ok("No files found matching formatter patterns".to_string());
    }

    info!(
        total = files.total_count(),
        "Found files to {}",
        mode.verb()
    );

    let environments = FormatterEnvironments::resolve(
        ProjectEnvironmentRequest {
            path,
            package,
            environment_override,
        },
        &commands_to_run(&files, &formatters),
        executor,
    )
    .await?;
    let context = FormatterContext {
        project_root: &project_root,
        dry_run: cuenv_core::DryRun::No,
        mode,
        environments: &environments,
    };
    let mut results = Vec::new();
    let mut errors = Vec::new();

    // Run Rust formatter
    if !files.rust.is_empty() {
        let file_refs: Vec<&Path> = files.rust.iter().map(AsRef::as_ref).collect();
        match run_rust_formatter(&file_refs, formatters.rust.as_ref(), &context) {
            Ok(result) => results.push(result),
            Err(e) => errors.push(e),
        }
    }

    // Run Nix formatter
    if !files.nix.is_empty() {
        let file_refs: Vec<&Path> = files.nix.iter().map(AsRef::as_ref).collect();
        match run_nix_formatter(&file_refs, formatters.nix.as_ref(), &context) {
            Ok(result) => results.push(result),
            Err(e) => errors.push(e),
        }
    }

    // Run Go formatter
    if !files.go.is_empty() {
        let file_refs: Vec<&Path> = files.go.iter().map(AsRef::as_ref).collect();
        match run_go_formatter(&file_refs, &context) {
            Ok(result) => results.push(result),
            Err(e) => errors.push(e),
        }
    }

    // Run CUE formatter
    if !files.cue.is_empty() {
        let file_refs: Vec<&Path> = files.cue.iter().map(AsRef::as_ref).collect();
        match run_cue_formatter(&file_refs, &context) {
            Ok(result) => results.push(result),
            Err(e) => errors.push(e),
        }
    }

    // Return first error if any occurred
    if let Some(first_error) = errors.into_iter().next() {
        // In check mode, add helpful message about --fix
        if mode.is_check() {
            let error_msg = format!("{first_error}");
            return Err(cuenv_core::Error::configuration(format!(
                "{error_msg}\n\nRun `cuenv fmt --fix` to apply changes."
            )));
        }
        return Err(first_error);
    }

    if results.is_empty() {
        Ok("No files needed formatting".to_string())
    } else {
        Ok(results.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_include_no_filter() {
        assert!(should_include("rust", None));
        assert!(should_include("nix", None));
        assert!(should_include("go", None));
        assert!(should_include("cue", None));
    }

    #[test]
    fn test_should_include_with_filter() {
        let only = vec!["rust".to_string(), "go".to_string()];
        assert!(should_include("rust", Some(&only)));
        assert!(should_include("go", Some(&only)));
        assert!(!should_include("nix", Some(&only)));
        assert!(!should_include("cue", Some(&only)));
    }

    #[test]
    fn test_should_include_case_insensitive() {
        let only = vec!["RUST".to_string(), "Go".to_string()];
        assert!(should_include("rust", Some(&only)));
        assert!(should_include("Rust", Some(&only)));
        assert!(should_include("go", Some(&only)));
        assert!(should_include("GO", Some(&only)));
    }

    #[test]
    fn test_discovered_files_is_empty() {
        let empty = DiscoveredFiles {
            rust: vec![],
            nix: vec![],
            go: vec![],
            cue: vec![],
        };
        assert!(empty.is_empty());
        assert_eq!(empty.total_count(), 0);

        let non_empty = DiscoveredFiles {
            rust: vec![PathBuf::from("test.rs")],
            nix: vec![],
            go: vec![],
            cue: vec![],
        };
        assert!(!non_empty.is_empty());
        assert_eq!(non_empty.total_count(), 1);
    }

    #[test]
    fn test_discovered_files_total_count_all_types() {
        let files = DiscoveredFiles {
            rust: vec![PathBuf::from("a.rs"), PathBuf::from("b.rs")],
            nix: vec![PathBuf::from("flake.nix")],
            go: vec![PathBuf::from("main.go"), PathBuf::from("util.go")],
            cue: vec![PathBuf::from("env.cue")],
        };
        assert_eq!(files.total_count(), 6);
        assert!(!files.is_empty());
    }

    #[test]
    fn test_discovered_files_single_type_not_empty() {
        // Test each type individually
        let rust_only = DiscoveredFiles {
            rust: vec![PathBuf::from("lib.rs")],
            nix: vec![],
            go: vec![],
            cue: vec![],
        };
        assert!(!rust_only.is_empty());

        let nix_only = DiscoveredFiles {
            rust: vec![],
            nix: vec![PathBuf::from("shell.nix")],
            go: vec![],
            cue: vec![],
        };
        assert!(!nix_only.is_empty());

        let go_only = DiscoveredFiles {
            rust: vec![],
            nix: vec![],
            go: vec![PathBuf::from("main.go")],
            cue: vec![],
        };
        assert!(!go_only.is_empty());

        let cue_only = DiscoveredFiles {
            rust: vec![],
            nix: vec![],
            go: vec![],
            cue: vec![PathBuf::from("env.cue")],
        };
        assert!(!cue_only.is_empty());
    }

    #[test]
    fn test_should_include_empty_filter_list() {
        let empty: Vec<String> = vec![];
        // Empty list means nothing matches
        assert!(!should_include("rust", Some(&empty)));
        assert!(!should_include("nix", Some(&empty)));
    }

    #[test]
    fn test_should_include_single_formatter() {
        let only = vec!["cue".to_string()];
        assert!(!should_include("rust", Some(&only)));
        assert!(!should_include("nix", Some(&only)));
        assert!(!should_include("go", Some(&only)));
        assert!(should_include("cue", Some(&only)));
    }

    fn test_executor() -> CommandExecutor {
        let (sender, _receiver) = tokio::sync::mpsc::unbounded_channel();
        CommandExecutor::new(sender, "cuenv".to_string())
    }

    fn check_request(path: &str) -> FmtRequest<'_> {
        FmtRequest {
            path,
            package: "cuenv",
            mode: FormatMode::Check,
            only: None,
            environment_override: None,
        }
    }

    #[tokio::test]
    async fn test_execute_fmt_invalid_path() {
        let result = execute_fmt(check_request("/nonexistent/path"), &test_executor()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_execute_fmt_no_cue_module() {
        // Use temp dir without cue.mod
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().to_string_lossy().into_owned();
        let result = execute_fmt(check_request(&path), &test_executor()).await;
        assert!(result.is_err());
    }

    #[test]
    fn test_load_base_config_invalid_path() {
        let result = load_base_config("/nonexistent/path", &test_executor());
        assert!(result.is_err());
    }

    #[test]
    fn test_commands_to_run_only_formatters_with_files() {
        let formatters = Formatters {
            nix: Some(cuenv_core::manifest::NixFormatter {
                enabled: true,
                includes: vec!["*.nix".to_string()],
                tool: NixFormatterTool::Alejandra,
            }),
            ..Default::default()
        };
        let files = DiscoveredFiles {
            rust: vec![PathBuf::from("lib.rs")],
            nix: vec![PathBuf::from("flake.nix")],
            go: vec![],
            cue: vec![],
        };
        assert_eq!(
            commands_to_run(&files, &formatters),
            ["rustfmt", "alejandra"]
        );
    }

    /// End-to-end: `execute_fmt` resolves `rustfmt` against the `PATH` the
    /// project declares in `env.cue`, not cuenv's inherited `PATH`.
    #[cfg(unix)]
    mod project_environment {
        use super::*;
        use crate::commands::sync::formatters::test_support::install_test_script;
        use tempfile::TempDir;

        /// A CUE module whose `env.PATH` points at a fake `rustfmt`.
        fn project_with_fake_rustfmt(script: &str) -> TempDir {
            let dir = TempDir::new().unwrap();
            let root = dir.path();
            let bin = root.join("bin");
            std::fs::create_dir_all(root.join("cue.mod")).unwrap();
            std::fs::create_dir(&bin).unwrap();
            std::fs::write(
                root.join("cue.mod/module.cue"),
                "module: \"example.com/fmt\"\nlanguage: version: \"v0.14.1\"\n",
            )
            .unwrap();
            std::fs::write(
                root.join("env.cue"),
                format!(
                    "package cuenv\n\nenv: {{\n\tPATH: \"{}\"\n\tFAKE_FORMATTER_MARKER: \"{}\"\n}}\n\nformatters: rust: {{}}\n",
                    bin.display(),
                    root.join("invoked").display()
                ),
            )
            .unwrap();
            std::fs::write(root.join("lib.rs"), "fn main() {}\n").unwrap();
            install_test_script(&bin.join("rustfmt"), script);
            dir
        }

        fn fix_request(path: &str) -> FmtRequest<'_> {
            FmtRequest {
                mode: FormatMode::Fix,
                ..check_request(path)
            }
        }

        #[tokio::test]
        async fn fix_mode_uses_project_path_rustfmt() {
            let project = project_with_fake_rustfmt("echo \"$@\" > \"$FAKE_FORMATTER_MARKER\"");
            let path = project.path().to_string_lossy().into_owned();

            let output = execute_fmt(fix_request(&path), &test_executor())
                .await
                .unwrap();

            assert_eq!(output, "Formatted 1 Rust file(s)");
            let recorded = std::fs::read_to_string(project.path().join("invoked")).unwrap();
            assert!(recorded.contains("lib.rs"), "{recorded}");
        }

        #[tokio::test]
        async fn fix_mode_fails_when_project_rustfmt_fails() {
            let project = project_with_fake_rustfmt("echo 'no toolchain' >&2\nexit 1");
            let path = project.path().to_string_lossy().into_owned();

            let error = execute_fmt(fix_request(&path), &test_executor())
                .await
                .expect_err("a failing rustfmt must fail `cuenv fmt --fix`");

            let message = error.to_string();
            assert!(message.contains("rustfmt failed"), "{message}");
            assert!(message.contains("no toolchain"), "{message}");
        }
    }
}
