//! Tests that evaluate this repository's own CUE files through the bridge.
//!
//! The repository is the largest real CUE module the bridge sees in tests:
//! every example and schema file must keep evaluating.
//!
//! Each test copies the CUE files it needs into a temporary module instead
//! of evaluating the checkout in place. The checkout's `target/` directory
//! holds build output and fixtures that other test binaries write while
//! this one runs, which a recursive load would otherwise pick up.

use cuengine::{InstanceFailures, ModuleEvalOptions, PackageScope, evaluate_module};
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

fn repository_root() -> TestResult<PathBuf> {
    Ok(Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?)
}

fn temporary_module(prefix: &str) -> TestResult<TempDir> {
    Ok(tempfile::Builder::new().prefix(prefix).tempdir()?)
}

/// Directories never copied: build output, dependencies, and trees the CUE
/// loader does not walk anyway (names starting with `.` or `_`).
fn is_copied_directory(name: &str) -> bool {
    !(name == "target" || name == "node_modules" || name.starts_with('.') || name.starts_with('_'))
}

/// Copy every `.cue` file below `source` (and all of `cue.mod`) to
/// `destination`, keeping relative paths.
fn copy_cue_files(source: &Path, destination: &Path) -> TestResult {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            if name == "cue.mod" || is_copied_directory(&name) {
                copy_cue_files(&path, &destination.join(name.as_ref()))?;
            }
        } else if file_type.is_file()
            && (path.extension().is_some_and(|extension| extension == "cue")
                || source.ends_with("cue.mod"))
        {
            fs::create_dir_all(destination)?;
            fs::copy(&path, destination.join(name.as_ref()))?;
        }
    }
    Ok(())
}

#[test]
fn every_instance_of_the_repository_evaluates() -> TestResult {
    // The strictest module-wide evaluation: every package of every
    // directory, and any failure fails the call. A broken example (for example a task reference that no longer
    // resolves) makes this test fail instead of landing silently.
    let repository = repository_root()?;
    let module = temporary_module("cuengine-repository-")?;
    copy_cue_files(&repository, module.path())?;

    let options = ModuleEvalOptions {
        recursive: true,
        package_scope: PackageScope::All,
        instance_failures: InstanceFailures::Fail,
        ..Default::default()
    };
    let result = evaluate_module(module.path(), "", Some(&options))?;
    assert!(
        result.instances.contains_key(".:cuenv"),
        "the repository's own env.cue is missing: {:?}",
        result.instances.keys().collect::<Vec<_>>()
    );
    assert!(
        result
            .instances
            .keys()
            .any(|key| key.starts_with("examples/")),
        "no example was evaluated"
    );
    Ok(())
}
