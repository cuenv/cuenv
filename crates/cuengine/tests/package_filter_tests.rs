//! Regression tests for CUE package filtering.

use cuengine::{ModuleEvalOptions, evaluate_module};
use serde_json::Value;
use std::error::Error;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

fn create_module() -> TestResult<TempDir> {
    let temp_dir = tempfile::Builder::new()
        .prefix("cuengine-package-filter-")
        .tempdir()?;
    let root = temp_dir.path();

    fs::create_dir_all(root.join("cue.mod"))?;
    fs::write(
        root.join("cue.mod/module.cue"),
        "module: \"example.com/package-filter\"\nlanguage: {\n\tversion: \"v0.9.0\"\n}\n",
    )?;

    Ok(temp_dir)
}

#[test]
fn recursive_eval_ignores_other_packages_in_same_directory() -> TestResult {
    let temp_dir = create_module()?;
    let root = temp_dir.path();

    fs::write(
        root.join("env.cue"),
        r#"package cuenv

env: {
  KEEP: "yes"
}
"#,
    )?;
    fs::write(
        root.join("another.cue"),
        r#"package random

ignored: "this belongs to another CUE package"
"#,
    )?;

    let options = ModuleEvalOptions {
        recursive: true,
        package_name: Some("cuenv".to_string()),
        ..Default::default()
    };
    let result = evaluate_module(root, "cuenv", Some(&options))?;

    assert_eq!(result.instances.len(), 1);
    let root_instance = result
        .instances
        .get(".")
        .ok_or_else(|| std::io::Error::other("missing root cuenv instance"))?;
    assert_eq!(
        root_instance["env"]["KEEP"],
        Value::String("yes".to_string())
    );
    assert!(root_instance.get("ignored").is_none());

    Ok(())
}

#[test]
fn filtered_evaluation_reports_no_matching_package_as_empty() -> TestResult {
    let temp_dir = create_module()?;
    let root = temp_dir.path();
    fs::write(
        root.join("other.cue"),
        "package unrelated\nvalue: \"not cuetty\"\n",
    )?;

    let options = ModuleEvalOptions {
        recursive: false,
        package_name: Some("cuetty".to_string()),
        ..Default::default()
    };
    let result = evaluate_module(root, "cuetty", Some(&options))?;

    assert!(result.instances.is_empty());
    Ok(())
}

#[test]
fn filtered_evaluation_accepts_relative_module_roots() -> TestResult {
    let temp_dir = tempfile::Builder::new()
        .prefix("cuengine-relative-package-filter-")
        .tempdir_in(".")?;
    let root = PathBuf::from(".").join(
        temp_dir
            .path()
            .file_name()
            .expect("temp directory has a filename"),
    );
    fs::create_dir_all(root.join("cue.mod"))?;
    fs::write(
        root.join("cue.mod/module.cue"),
        "module: \"example.com/relative-package-filter\"\nlanguage: {\n\tversion: \"v0.9.0\"\n}\n",
    )?;
    fs::write(
        root.join("terminal.cue"),
        "package cuetty\nbanner: \"ok\"\n",
    )?;
    fs::write(
        root.join("other.cue"),
        "package unrelated\nvalue: \"ignored\"\n",
    )?;

    assert!(!root.is_absolute());
    let options = ModuleEvalOptions {
        recursive: false,
        package_name: Some("cuetty".to_string()),
        target_dir: Some(root.to_string_lossy().into_owned()),
        ..Default::default()
    };
    let result = evaluate_module(&root, "cuetty", Some(&options))?;
    assert_eq!(result.instances.len(), 1);
    assert_eq!(result.instances["."]["banner"], Value::String("ok".into()));
    Ok(())
}
