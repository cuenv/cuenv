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
        root.join("workspace.cue"),
        r#"package cuenv

env: {
  KEEP: "yes"
}
"#,
    )?;
    fs::write(
        root.join("unrelated.cue"),
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
fn recursive_eval_classifies_projects_from_the_serialized_name() -> TestResult {
    let temp_dir = create_module()?;
    let root = temp_dir.path();

    fs::write(
        root.join("defaults.cue"),
        r#"package cuenv

env: {
  ROOT: "yes"
}
"#,
    )?;

    let chat = root.join("chat");
    fs::create_dir_all(&chat)?;
    fs::write(
        chat.join("project.cue"),
        r#"package cuenv

name: "chat"
"#,
    )?;
    fs::create_dir_all(root.join("docs/assets"))?;
    let tools = root.join("tools");
    fs::create_dir_all(&tools)?;
    fs::write(
        tools.join("helper.cue"),
        r#"package helper

value: "unrelated"
"#,
    )?;

    let options = ModuleEvalOptions {
        recursive: true,
        package_name: Some("cuenv".to_string()),
        ..Default::default()
    };
    let result = evaluate_module(root, "cuenv", Some(&options))?;

    assert_eq!(result.instances.len(), 2);
    assert!(!result.instances.contains_key("docs"));
    assert!(!result.instances.contains_key("docs/assets"));
    assert!(!result.instances.contains_key("tools"));
    assert_eq!(result.projects, vec!["chat".to_string()]);
    assert_eq!(
        result
            .instances
            .get("chat")
            .and_then(|value| value.get("name")),
        Some(&Value::String("chat".to_string()))
    );

    Ok(())
}

#[test]
fn recursive_eval_rejects_partial_selected_package() -> TestResult {
    let temp_dir = create_module()?;
    let root = temp_dir.path();

    let api = root.join("api");
    fs::create_dir_all(&api)?;
    fs::write(
        api.join("project.cue"),
        r#"package cuenv

name: "api"
"#,
    )?;

    let chat = root.join("chat");
    fs::create_dir_all(&chat)?;
    fs::write(
        chat.join("configuration.cue"),
        r#"package cuenv

name: "chat"
broken: 1 & 2
"#,
    )?;

    let options = ModuleEvalOptions {
        recursive: true,
        package_name: Some("cuenv".to_string()),
        ..Default::default()
    };
    let error = evaluate_module(root, "cuenv", Some(&options))
        .expect_err("one broken selected-package instance must fail the entire evaluation");
    let message = error.to_string();
    assert!(
        message.contains("Failed to build selected CUE package"),
        "unexpected error: {message}"
    );
    assert!(message.contains("chat"), "unexpected error: {message}");

    Ok(())
}

#[test]
fn recursive_eval_rejects_selected_package_load_error() -> TestResult {
    let temp_dir = create_module()?;
    let root = temp_dir.path();

    let api = root.join("api");
    fs::create_dir_all(&api)?;
    fs::write(
        api.join("project.cue"),
        r#"package cuenv

name: "api"
"#,
    )?;

    let chat = root.join("chat");
    fs::create_dir_all(&chat)?;
    fs::write(
        chat.join("configuration.cue"),
        r#"package cuenv

name: "chat"
broken: {
"#,
    )?;

    let options = ModuleEvalOptions {
        recursive: true,
        package_name: Some("cuenv".to_string()),
        ..Default::default()
    };
    let error = evaluate_module(root, "cuenv", Some(&options))
        .expect_err("a selected-package load error must fail the entire evaluation");
    let message = error.to_string();
    assert!(
        message.contains("Failed to load selected CUE package"),
        "unexpected error: {message}"
    );
    assert!(message.contains("chat"), "unexpected error: {message}");

    Ok(())
}

#[test]
fn recursive_eval_ignores_attributed_unrelated_syntax_error() -> TestResult {
    let temp_dir = create_module()?;
    let root = temp_dir.path();

    fs::write(
        root.join("project.cue"),
        r#"package cuenv

name: "root"
"#,
    )?;
    fs::write(
        root.join("unrelated-syntax.cue"),
        r"package unrelated

broken: {
",
    )?;

    let options = ModuleEvalOptions {
        recursive: true,
        package_name: Some("cuenv".to_string()),
        ..Default::default()
    };
    let result = evaluate_module(root, "cuenv", Some(&options))?;

    assert_eq!(result.projects, vec![".".to_string()]);
    assert_eq!(result.instances.len(), 1);

    Ok(())
}

#[test]
fn recursive_eval_ignores_unimported_malformed_package_in_subdirectory() -> TestResult {
    let temp_dir = create_module()?;
    let root = temp_dir.path();

    fs::write(
        root.join("project.cue"),
        r#"package cuenv

name: "root"
"#,
    )?;

    let helper = root.join("helper");
    fs::create_dir_all(&helper)?;
    fs::write(
        helper.join("broken.cue"),
        r"package unrelated

broken: {
",
    )?;

    let options = ModuleEvalOptions {
        recursive: true,
        package_name: Some("cuenv".to_string()),
        ..Default::default()
    };
    let result = evaluate_module(root, "cuenv", Some(&options))?;

    assert_eq!(result.projects, vec![".".to_string()]);
    assert_eq!(result.instances.len(), 1);
    assert!(!result.instances.contains_key("helper"));

    Ok(())
}

#[test]
fn recursive_eval_rejects_malformed_imported_package() -> TestResult {
    let temp_dir = create_module()?;
    let root = temp_dir.path();

    fs::write(
        root.join("project.cue"),
        r#"package cuenv

import helper "example.com/package-filter/helper"

name: "root"
value: helper.value
"#,
    )?;

    let helper = root.join("helper");
    fs::create_dir_all(&helper)?;
    fs::write(
        helper.join("values.cue"),
        r#"package helper

value: "ok"
"#,
    )?;
    fs::write(
        helper.join("broken.cue"),
        r"package helper

broken: {
",
    )?;

    let options = ModuleEvalOptions {
        recursive: true,
        package_name: Some("cuenv".to_string()),
        ..Default::default()
    };
    let error = evaluate_module(root, "cuenv", Some(&options))
        .expect_err("a malformed imported package is part of selected-package evaluation");
    let message = error.to_string();
    assert!(
        message.contains("import failed"),
        "unexpected error: {message}"
    );

    Ok(())
}

#[test]
fn recursive_eval_rejects_syntax_error_without_recoverable_package() -> TestResult {
    let temp_dir = create_module()?;
    let root = temp_dir.path();

    fs::write(
        root.join("project.cue"),
        r#"package cuenv

name: "root"
"#,
    )?;
    fs::write(root.join("unknown.cue"), "broken: {\n")?;

    let options = ModuleEvalOptions {
        recursive: true,
        package_name: Some("cuenv".to_string()),
        ..Default::default()
    };
    let error = evaluate_module(root, "cuenv", Some(&options))
        .expect_err("a syntax error without a package clause must remain fail-closed");
    let message = error.to_string();
    assert!(
        message.contains("Failed to load selected CUE package or unattributed CUE input"),
        "unexpected error: {message}"
    );

    Ok(())
}

#[test]
fn recursive_eval_ignores_broken_unrelated_package() -> TestResult {
    let temp_dir = create_module()?;
    let root = temp_dir.path();

    fs::write(
        root.join("project.cue"),
        r#"package cuenv

name: "root"
"#,
    )?;
    fs::write(
        root.join("broken.cue"),
        r"package unrelated

broken: 1 & 2
",
    )?;

    let options = ModuleEvalOptions {
        recursive: true,
        package_name: Some("cuenv".to_string()),
        ..Default::default()
    };
    let result = evaluate_module(root, "cuenv", Some(&options))?;

    assert_eq!(result.projects, vec![".".to_string()]);
    assert_eq!(result.instances.len(), 1);
    assert_eq!(
        result.instances["."]["name"],
        Value::String("root".to_string())
    );

    Ok(())
}

#[test]
fn recursive_eval_ignores_unrelated_package_with_missing_import() -> TestResult {
    let temp_dir = create_module()?;
    let root = temp_dir.path();

    fs::write(
        root.join("project.cue"),
        r#"package cuenv

name: "root"
"#,
    )?;
    fs::write(
        root.join("unrelated-import.cue"),
        r#"package unrelated

import missing "example.com/package-filter/missing"

value: missing.value
"#,
    )?;

    let options = ModuleEvalOptions {
        recursive: true,
        package_name: Some("cuenv".to_string()),
        ..Default::default()
    };
    let result = evaluate_module(root, "cuenv", Some(&options))?;

    assert_eq!(result.projects, vec![".".to_string()]);
    assert_eq!(result.instances.len(), 1);

    Ok(())
}

#[test]
fn recursive_eval_rejects_unserializable_selected_instance() -> TestResult {
    let temp_dir = create_module()?;
    let root = temp_dir.path();

    let api = root.join("api");
    fs::create_dir_all(&api)?;
    fs::write(
        api.join("project.cue"),
        r#"package cuenv

name: "api"
"#,
    )?;

    let incomplete = root.join("incomplete");
    fs::create_dir_all(&incomplete)?;
    fs::write(
        incomplete.join("constraints.cue"),
        r#"package cuenv

name: "incomplete"
required: string
"#,
    )?;

    let options = ModuleEvalOptions {
        recursive: true,
        package_name: Some("cuenv".to_string()),
        ..Default::default()
    };
    let error = evaluate_module(root, "cuenv", Some(&options))
        .expect_err("an unserializable selected-package instance must fail the evaluation");
    let message = error.to_string();
    assert!(
        message.contains("Failed to serialize selected CUE package"),
        "unexpected error: {message}"
    );
    assert!(
        message.contains("incomplete"),
        "unexpected error: {message}"
    );

    Ok(())
}
