//! Tests for the `unify` evaluation option across the FFI boundary.

use cuengine::{ModuleEvalOptions, UnifySource};
use std::error::Error;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

const PACKAGE: &str = "app";

/// Creates a standalone directory holding one file of `package app`.
fn package_dir(body: &str) -> TestResult<TempDir> {
    let dir = tempfile::Builder::new()
        .prefix("cuengine-unify-")
        .tempdir()?;
    fs::write(
        dir.path().join("values.cue"),
        format!("package {PACKAGE}\n\n{body}\n"),
    )?;
    Ok(dir)
}

fn unify_options(target: &Path, sources: &[&Path]) -> ModuleEvalOptions {
    ModuleEvalOptions {
        package_name: Some(PACKAGE.to_string()),
        target_dir: Some(target.to_string_lossy().into_owned()),
        unify: sources
            .iter()
            .map(|dir| UnifySource {
                dir: dir.to_string_lossy().into_owned(),
                module_root: None,
            })
            .collect(),
        ..Default::default()
    }
}

fn evaluate(target: &Path, sources: &[&Path]) -> cuengine::Result<serde_json::Value> {
    let result = cuengine::evaluate_module(target, PACKAGE, Some(&unify_options(target, sources)))?;
    Ok(result.instances.get(".").cloned().unwrap_or_default())
}

#[test]
fn unify_merges_fields_and_lets_concrete_values_override_defaults() -> TestResult {
    let global = package_dir("colour: *\"blue\" | string\nshown: *true | bool\nglobal: 1")?;
    let local = package_dir("colour: \"red\"\nlocal: 2")?;

    let value = evaluate(local.path(), &[global.path()])?;
    assert_eq!(value["colour"], "red");
    assert_eq!(value["shown"], true);
    assert_eq!(value["global"], 1);
    assert_eq!(value["local"], 2);
    Ok(())
}

#[test]
fn unify_reports_conflicting_concrete_values() -> TestResult {
    let global = package_dir("colour: \"blue\"")?;
    let local = package_dir("colour: \"red\"")?;

    let error = evaluate(local.path(), &[global.path()])
        .err()
        .ok_or("conflicting sources must fail")?;
    assert!(error.to_string().contains("conflicting values"), "{error}");
    Ok(())
}

#[test]
fn unify_source_without_the_package_contributes_nothing() -> TestResult {
    let local = package_dir("colour: \"red\"")?;
    let empty = tempfile::tempdir()?;
    let missing = empty.path().join("missing");

    let value = evaluate(local.path(), &[empty.path(), &missing])?;
    assert_eq!(value, serde_json::json!({"colour": "red"}));
    Ok(())
}

#[test]
fn unify_source_with_a_syntax_error_fails_the_evaluation() -> TestResult {
    let global = package_dir("colour: {")?;
    let local = package_dir("colour: \"red\"")?;

    assert!(evaluate(local.path(), &[global.path()]).is_err());
    Ok(())
}

#[test]
fn unify_rejects_recursive_evaluation() -> TestResult {
    let global = package_dir("colour: \"blue\"")?;
    let local = package_dir("colour: \"red\"")?;
    let options = ModuleEvalOptions {
        recursive: true,
        ..unify_options(local.path(), &[global.path()])
    };

    assert!(cuengine::evaluate_module(local.path(), PACKAGE, Some(&options)).is_err());
    Ok(())
}
