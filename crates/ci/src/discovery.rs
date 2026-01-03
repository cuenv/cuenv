//! CI project discovery utilities.
//!
//! Provides functions for discovering CUE modules and projects for CI operations.
//! Projects can be discovered from the current directory or from an already-evaluated module.

use cuengine::ModuleEvalOptions;
use cuenv_core::ModuleEvaluation;
use cuenv_core::Result;
use std::path::{Path, PathBuf};

/// Find the CUE module root by walking up from `start` looking for `cue.mod/` directory.
#[must_use]
pub fn find_cue_module_root(start: &Path) -> Option<PathBuf> {
    let mut current = start.canonicalize().ok()?;
    loop {
        if current.join("cue.mod").is_dir() {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

/// Evaluate the CUE module from the current working directory.
///
/// This is a convenience function that finds the module root from CWD,
/// evaluates it, and returns the `ModuleEvaluation` for further processing.
///
/// # Errors
/// Returns an error if:
/// - Current directory cannot be determined
/// - Not inside a CUE module (no `cue.mod/` found)
/// - CUE evaluation fails
///
/// # Example
/// ```ignore
/// use cuenv_ci::discovery::evaluate_module_from_cwd;
/// use cuenv_core::manifest::Project;
///
/// let module = evaluate_module_from_cwd()?;
/// for instance in module.projects() {
///     let project = Project::try_from(instance)?;
///     println!("Found project: {}", project.name);
/// }
/// ```
pub fn evaluate_module_from_cwd() -> Result<ModuleEvaluation> {
    let cwd = std::env::current_dir().map_err(|e| cuenv_core::Error::Io {
        source: e,
        path: None,
        operation: "get current directory".to_string(),
    })?;

    let module_root = find_cue_module_root(&cwd).ok_or_else(|| {
        cuenv_core::Error::configuration(
            "Not inside a CUE module. Run 'cue mod init' or navigate to a directory with cue.mod/",
        )
    })?;

    let options = ModuleEvalOptions {
        recursive: true,
        ..Default::default()
    };
    let raw_result = cuengine::evaluate_module(&module_root, "cuenv", Some(&options))
        .map_err(|e| cuenv_core::Error::configuration(format!("CUE evaluation failed: {e}")))?;

    Ok(ModuleEvaluation::from_raw(
        module_root,
        raw_result.instances,
        raw_result.projects,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ==========================================================================
    // find_cue_module_root tests
    // ==========================================================================

    #[test]
    fn test_find_cue_module_root_from_nested_dir() {
        use std::fs;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let root = temp_dir.path();

        // Create cue.mod at root
        fs::create_dir_all(root.join("cue.mod")).expect("Failed to create cue.mod");

        // Create nested directory
        let nested = root.join("services").join("api");
        fs::create_dir_all(&nested).expect("Failed to create nested dir");

        // find_cue_module_root should find the root from nested
        let found = find_cue_module_root(&nested);
        assert!(found.is_some(), "Should find module root from nested dir");

        let found_path = found.unwrap();
        assert_eq!(
            found_path.canonicalize().unwrap(),
            root.canonicalize().unwrap(),
            "Found root should match actual root"
        );
    }

    #[test]
    fn test_find_cue_module_root_not_found() {
        use tempfile::TempDir;

        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        // No cue.mod directory

        let found = find_cue_module_root(temp_dir.path());
        assert!(
            found.is_none(),
            "Should not find module root without cue.mod"
        );
    }

    #[test]
    fn test_find_cue_module_root_from_root() {
        use std::fs;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let root = temp_dir.path();

        // Create cue.mod at root
        fs::create_dir_all(root.join("cue.mod")).expect("Failed to create cue.mod");

        // Find from root itself
        let found = find_cue_module_root(root);
        assert!(found.is_some());
    }

    #[test]
    fn test_find_cue_module_root_deeply_nested() {
        use std::fs;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let root = temp_dir.path();

        // Create cue.mod at root
        fs::create_dir_all(root.join("cue.mod")).expect("Failed to create cue.mod");

        // Create deeply nested directory
        let nested = root.join("a").join("b").join("c").join("d").join("e");
        fs::create_dir_all(&nested).expect("Failed to create nested dir");

        // Should still find module root
        let found = find_cue_module_root(&nested);
        assert!(found.is_some());
        assert_eq!(
            found.unwrap().canonicalize().unwrap(),
            root.canonicalize().unwrap()
        );
    }

    #[test]
    fn test_find_cue_module_root_cue_mod_file_not_dir() {
        use std::fs;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let root = temp_dir.path();

        // Create cue.mod as a FILE (not directory) - should not be recognized
        fs::write(root.join("cue.mod"), "not a directory").expect("Failed to create file");

        let found = find_cue_module_root(root);
        assert!(
            found.is_none(),
            "File named cue.mod should not count as module root"
        );
    }

    #[test]
    fn test_find_cue_module_root_intermediate_cue_mod() {
        use std::fs;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().expect("Failed to create temp dir");
        let root = temp_dir.path();

        // Create cue.mod in an intermediate directory
        let intermediate = root.join("project");
        fs::create_dir_all(intermediate.join("cue.mod")).expect("Failed to create cue.mod");

        // Create deeply nested directory
        let nested = intermediate.join("services").join("api");
        fs::create_dir_all(&nested).expect("Failed to create nested dir");

        // Should find the intermediate cue.mod, not walk all the way up
        let found = find_cue_module_root(&nested);
        assert!(found.is_some());
        assert_eq!(
            found.unwrap().canonicalize().unwrap(),
            intermediate.canonicalize().unwrap()
        );
    }

    #[test]
    fn test_find_cue_module_root_nonexistent_path() {
        let path = PathBuf::from("/nonexistent/path/that/does/not/exist");
        let found = find_cue_module_root(&path);
        assert!(found.is_none());
    }
}
