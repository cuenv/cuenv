//! CI Pipeline Runner Module
//!
//! Executes CI pipelines locally or in CI environments with:
//! - Parallel task execution with bounded concurrency
//! - Progress reporting (terminal or GitHub Check Runs)
//! - Matrix filtering for cross-platform support

use super::args::CiArgs;
use crate::providers::detect_ci_provider;
use cuenv_ci::discovery::find_cue_module_root;
use cuenv_ci::executor::run_ci;
use cuenv_core::Result;

/// Execute runner mode - run the pipeline.
///
/// # Arguments
/// * `args` - CLI arguments
///
/// # Errors
///
/// Returns error if pipeline execution fails.
pub async fn execute_runner(args: &CiArgs) -> Result<()> {
    let provider = detect_ci_provider(args.from.clone());

    // TODO: Apply matrix filter if specified
    if !args.filter_matrix.is_empty() {
        tracing::info!(
            filter = ?args.filter_matrix,
            "Matrix filter specified (not yet fully implemented)"
        );
    }

    // TODO: Apply jobs limit
    if args.jobs != 0 {
        tracing::info!(
            jobs = args.effective_jobs(),
            "Parallel jobs limit specified"
        );
    }

    // Resolve --path relative to cwd and then relative to module root.
    // This ensures that `--path .` from a subdirectory correctly filters to that project.
    let effective_path = resolve_path_filter(&args.path)?;

    // For now, delegate to existing run_ci with parallel execution
    // This will be replaced with the new ExecutionEngine in Phase 1 Part 2
    run_ci(
        provider,
        args.dry_run.into(),
        args.pipeline.clone(),
        args.environment.clone(),
        effective_path.as_deref(),
    )
    .await
}

/// Resolve the path filter to be relative to the module root.
///
/// When the user specifies `--path .`, this should mean "the current directory"
/// relative to the module root, not "no filter".
fn resolve_path_filter(path: &str) -> Result<Option<String>> {
    let cwd = std::env::current_dir().map_err(|e| cuenv_core::Error::Io {
        source: e,
        path: None,
        operation: "get current directory".to_string(),
    })?;

    let Some(module_root) = find_cue_module_root(&cwd) else {
        // If we can't find the module root, let run_ci handle it (will fail with proper error)
        return Ok(Some(path.to_string()));
    };

    // Canonicalize both paths for reliable comparison
    let cwd_canon = cwd.canonicalize().map_err(|e| cuenv_core::Error::Io {
        source: e,
        path: Some(cwd.clone().into_boxed_path()),
        operation: "canonicalize current directory".to_string(),
    })?;

    let module_root_canon = module_root
        .canonicalize()
        .map_err(|e| cuenv_core::Error::Io {
            source: e,
            path: Some(module_root.clone().into_boxed_path()),
            operation: "canonicalize module root".to_string(),
        })?;

    // If at the module root, "." means "all projects" (no filter)
    if cwd_canon == module_root_canon && path == "." {
        return Ok(None);
    }

    // If path is ".", resolve it to cwd relative to module root
    if path == "." {
        let relative = cwd_canon.strip_prefix(&module_root_canon).map_err(|_| {
            cuenv_core::Error::configuration(format!(
                "Current directory '{}' is not inside module root '{}'",
                cwd.display(),
                module_root.display()
            ))
        })?;
        return Ok(Some(relative.to_string_lossy().to_string()));
    }

    // For explicit paths, resolve relative to cwd, then relative to module root
    let absolute_path = if std::path::Path::new(path).is_absolute() {
        std::path::PathBuf::from(path)
    } else {
        cwd.join(path)
    };

    if !absolute_path.exists() {
        return Err(cuenv_core::Error::configuration(format!(
            "Path '{}' does not exist",
            path
        )));
    }

    let absolute_canon = absolute_path
        .canonicalize()
        .map_err(|e| cuenv_core::Error::Io {
            source: e,
            path: Some(absolute_path.clone().into_boxed_path()),
            operation: "canonicalize path".to_string(),
        })?;

    let relative = absolute_canon
        .strip_prefix(&module_root_canon)
        .map_err(|_| {
            cuenv_core::Error::configuration(format!(
                "Path '{}' is not inside module root '{}'",
                path,
                module_root.display()
            ))
        })?;

    // Return the path relative to module root
    Ok(Some(relative.to_string_lossy().to_string()))
}
