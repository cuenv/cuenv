//! Format command implementation.
//!
//! Formats all files in a repository based on the formatters configuration
//! defined in `env.cue`. Uses the same formatter runners as the sync command
//! but discovers files by walking the directory tree.

use crate::commands::env_file::find_cue_module_root;
use crate::commands::sync::{
    matches_any_pattern, run_cue_formatter, run_go_formatter, run_nix_formatter, run_rust_formatter,
};
use crate::commands::{convert_engine_error, relative_path_from_root};
use cuengine::ModuleEvalOptions;
use cuenv_core::manifest::{Base, Formatters};
use cuenv_core::{ModuleEvaluation, Result};
use ignore::WalkBuilder;
use std::path::{Path, PathBuf};
use tracing::{debug, info};

/// Files discovered for each formatter type.
#[allow(clippy::struct_field_names)]
struct DiscoveredFiles {
    rust_files: Vec<PathBuf>,
    nix_files: Vec<PathBuf>,
    go_files: Vec<PathBuf>,
    cue_files: Vec<PathBuf>,
}

impl DiscoveredFiles {
    fn is_empty(&self) -> bool {
        self.rust_files.is_empty()
            && self.nix_files.is_empty()
            && self.go_files.is_empty()
            && self.cue_files.is_empty()
    }

    fn total_count(&self) -> usize {
        self.rust_files.len() + self.nix_files.len() + self.go_files.len() + self.cue_files.len()
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
        rust_files,
        nix_files,
        go_files,
        cue_files,
    }
}

/// Load the Base configuration from the CUE module.
fn load_base_config(path: &str, package: &str) -> Result<Base> {
    let target_path = Path::new(path)
        .canonicalize()
        .map_err(|e| cuenv_core::Error::Io {
            source: e,
            path: Some(Path::new(path).to_path_buf().into_boxed_path()),
            operation: "canonicalize path".to_string(),
        })?;

    let module_root = find_cue_module_root(&target_path).ok_or_else(|| {
        cuenv_core::Error::configuration(format!(
            "No CUE module found (looking for cue.mod/) starting from: {}",
            target_path.display()
        ))
    })?;

    let options = ModuleEvalOptions {
        recursive: false,
        target_dir: Some(target_path.to_string_lossy().to_string()),
        ..Default::default()
    };
    let raw_result = cuengine::evaluate_module(&module_root, package, Some(&options))
        .map_err(convert_engine_error)?;

    let module = ModuleEvaluation::from_raw(
        module_root.clone(),
        raw_result.instances,
        raw_result.projects,
        None,
    );

    let relative_path = relative_path_from_root(&module_root, &target_path);
    let instance = module.get(&relative_path).ok_or_else(|| {
        cuenv_core::Error::configuration(format!(
            "No CUE instance found at path: {} (relative: {})",
            target_path.display(),
            relative_path.display()
        ))
    })?;

    instance.deserialize()
}

/// Execute the format command.
///
/// # Arguments
/// * `path` - Path to the CUE module directory
/// * `package` - CUE package name to evaluate
/// * `fix` - If true, apply formatting changes; if false, check mode (validate only)
/// * `only` - Optional list of formatters to run (e.g., `["rust", "go"]`)
///
/// # Returns
/// A string describing the formatting results.
///
/// # Errors
/// Returns an error if:
/// - No formatters are configured in env.cue
/// - In check mode and files need formatting
/// - A formatter command fails to execute
pub fn execute_fmt(
    path: &str,
    package: &str,
    fix: bool,
    only: Option<&[String]>,
) -> Result<String> {
    // Load config
    let config = load_base_config(path, package)?;

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
    let project_root = Path::new(path)
        .canonicalize()
        .map_err(|e| cuenv_core::Error::Io {
            source: e,
            path: Some(Path::new(path).to_path_buf().into_boxed_path()),
            operation: "canonicalize path".to_string(),
        })?;

    let files = discover_files(&project_root, &formatters, only);

    if files.is_empty() {
        return Ok("No files found matching formatter patterns".to_string());
    }

    info!(
        total = files.total_count(),
        "Found files to {}",
        if fix { "format" } else { "check" }
    );

    // Run formatters (check = !fix)
    let check = !fix;
    let dry_run = false;
    let mut results = Vec::new();
    let mut errors = Vec::new();

    // Run Rust formatter
    if !files.rust_files.is_empty() {
        let file_refs: Vec<&Path> = files.rust_files.iter().map(AsRef::as_ref).collect();
        match run_rust_formatter(
            &file_refs,
            formatters.rust.as_ref(),
            &project_root,
            dry_run,
            check,
        ) {
            Ok(result) => results.push(result),
            Err(e) => errors.push(e),
        }
    }

    // Run Nix formatter
    if !files.nix_files.is_empty() {
        let file_refs: Vec<&Path> = files.nix_files.iter().map(AsRef::as_ref).collect();
        match run_nix_formatter(
            &file_refs,
            formatters.nix.as_ref(),
            &project_root,
            dry_run,
            check,
        ) {
            Ok(result) => results.push(result),
            Err(e) => errors.push(e),
        }
    }

    // Run Go formatter
    if !files.go_files.is_empty() {
        let file_refs: Vec<&Path> = files.go_files.iter().map(AsRef::as_ref).collect();
        match run_go_formatter(&file_refs, &project_root, dry_run, check) {
            Ok(result) => results.push(result),
            Err(e) => errors.push(e),
        }
    }

    // Run CUE formatter
    if !files.cue_files.is_empty() {
        let file_refs: Vec<&Path> = files.cue_files.iter().map(AsRef::as_ref).collect();
        match run_cue_formatter(&file_refs, &project_root, dry_run, check) {
            Ok(result) => results.push(result),
            Err(e) => errors.push(e),
        }
    }

    // Return first error if any occurred
    if let Some(first_error) = errors.into_iter().next() {
        // In check mode, add helpful message about --fix
        if check {
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
            rust_files: vec![],
            nix_files: vec![],
            go_files: vec![],
            cue_files: vec![],
        };
        assert!(empty.is_empty());
        assert_eq!(empty.total_count(), 0);

        let non_empty = DiscoveredFiles {
            rust_files: vec![PathBuf::from("test.rs")],
            nix_files: vec![],
            go_files: vec![],
            cue_files: vec![],
        };
        assert!(!non_empty.is_empty());
        assert_eq!(non_empty.total_count(), 1);
    }

    #[test]
    fn test_discovered_files_total_count_all_types() {
        let files = DiscoveredFiles {
            rust_files: vec![PathBuf::from("a.rs"), PathBuf::from("b.rs")],
            nix_files: vec![PathBuf::from("flake.nix")],
            go_files: vec![PathBuf::from("main.go"), PathBuf::from("util.go")],
            cue_files: vec![PathBuf::from("env.cue")],
        };
        assert_eq!(files.total_count(), 6);
        assert!(!files.is_empty());
    }

    #[test]
    fn test_discovered_files_single_type_not_empty() {
        // Test each type individually
        let rust_only = DiscoveredFiles {
            rust_files: vec![PathBuf::from("lib.rs")],
            nix_files: vec![],
            go_files: vec![],
            cue_files: vec![],
        };
        assert!(!rust_only.is_empty());

        let nix_only = DiscoveredFiles {
            rust_files: vec![],
            nix_files: vec![PathBuf::from("shell.nix")],
            go_files: vec![],
            cue_files: vec![],
        };
        assert!(!nix_only.is_empty());

        let go_only = DiscoveredFiles {
            rust_files: vec![],
            nix_files: vec![],
            go_files: vec![PathBuf::from("main.go")],
            cue_files: vec![],
        };
        assert!(!go_only.is_empty());

        let cue_only = DiscoveredFiles {
            rust_files: vec![],
            nix_files: vec![],
            go_files: vec![],
            cue_files: vec![PathBuf::from("env.cue")],
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

    #[test]
    fn test_execute_fmt_invalid_path() {
        let result = execute_fmt("/nonexistent/path", "cuenv", false, None);
        assert!(result.is_err());
    }

    #[test]
    fn test_execute_fmt_no_cue_module() {
        // Use temp dir without cue.mod
        let temp = std::env::temp_dir();
        let result = execute_fmt(temp.to_str().unwrap(), "cuenv", false, None);
        assert!(result.is_err());
    }

    #[test]
    fn test_load_base_config_invalid_path() {
        let result = load_base_config("/nonexistent/path", "cuenv");
        assert!(result.is_err());
    }
}
