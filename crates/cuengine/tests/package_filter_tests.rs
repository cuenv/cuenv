//! Regression tests for CUE package filtering.

use cuengine::ModuleEvalOptions;
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
fn single_package_uses_parent_module_for_imports() -> TestResult {
    let temp_dir = TempDir::new()?;
    let root = temp_dir.path();
    fs::create_dir_all(root.join("cue.mod"))?;
    fs::create_dir_all(root.join("shared"))?;
    fs::create_dir_all(root.join("apps/selected"))?;
    fs::create_dir_all(root.join("apps/other"))?;
    fs::write(
        root.join("cue.mod/module.cue"),
        "module: \"example.com/parent@v0\"\nlanguage: version: \"v0.14.1\"\n",
    )?;
    fs::write(
        root.join("shared/values.cue"),
        "package shared\n#Value: 42\n",
    )?;
    fs::write(
        root.join("apps/selected/env.cue"),
        "package app\nimport \"example.com/parent/shared\"\nanswer: shared.#Value\n",
    )?;
    fs::write(root.join("apps/other/env.cue"), "package app\nanswer: 7\n")?;

    let json = cuengine::evaluate_cue_package(&root.join("apps/selected"), "app")?;
    let value: Value = serde_json::from_str(&json)?;
    assert_eq!(value["answer"], 42);
    Ok(())
}

#[test]
fn single_package_prefers_nearest_nested_module() -> TestResult {
    let temp_dir = TempDir::new()?;
    let root = temp_dir.path();
    fs::create_dir_all(root.join("cue.mod"))?;
    fs::create_dir_all(root.join("nested/cue.mod"))?;
    fs::create_dir_all(root.join("nested/shared"))?;
    fs::create_dir_all(root.join("nested/app"))?;
    fs::write(
        root.join("cue.mod/module.cue"),
        "module: \"example.com/outer@v0\"\nlanguage: version: \"v0.14.1\"\n",
    )?;
    fs::write(
        root.join("nested/cue.mod/module.cue"),
        "module: \"example.com/nested@v0\"\nlanguage: version: \"v0.14.1\"\n",
    )?;
    fs::write(
        root.join("nested/shared/values.cue"),
        "package shared\n#Value: 99\n",
    )?;
    fs::write(
        root.join("nested/app/env.cue"),
        "package app\nimport \"example.com/nested/shared\"\nanswer: shared.#Value\n",
    )?;

    let json = cuengine::evaluate_cue_package(&root.join("nested/app"), "app")?;
    let value: Value = serde_json::from_str(&json)?;
    assert_eq!(value["answer"], 99);
    Ok(())
}

#[test]
fn single_package_accepts_standalone_directory() -> TestResult {
    let temp_dir = TempDir::new()?;
    fs::write(temp_dir.path().join("env.cue"), "package app\nanswer: 17\n")?;
    let json = cuengine::evaluate_cue_package(temp_dir.path(), "app")?;
    let value: Value = serde_json::from_str(&json)?;
    assert_eq!(value["answer"], 17);
    Ok(())
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
    let result = cuengine::evaluate_module(root, "cuenv", Some(&options))?;

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
    let result = cuengine::evaluate_module(root, "cuetty", Some(&options))?;

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
    let result = cuengine::evaluate_module(&root, "cuetty", Some(&options))?;
    assert_eq!(result.instances.len(), 1);
    assert_eq!(result.instances["."]["banner"], Value::String("ok".into()));
    Ok(())
}
