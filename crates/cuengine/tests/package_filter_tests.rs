//! Regression tests for CUE package filtering.

use cuengine::{ModuleEvalOptions, evaluate_module};
use serde_json::Value;
use std::error::Error;
use std::fs;
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
