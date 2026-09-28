//! Tests for the `concrete_paths` and `instance_failures` evaluation
//! options across the FFI boundary.

use cuengine::{
    CueEngineError, InstanceFailures, ModuleEvalOptions, ModuleResult, evaluate_module,
};
use std::error::Error;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

const PACKAGE: &str = "app";

/// Creates a module; `files` maps a relative path to the package body.
fn create_module(files: &[(&str, &str)]) -> TestResult<TempDir> {
    let temp_dir = tempfile::Builder::new()
        .prefix("cuengine-evaluation-options-")
        .tempdir()?;
    let root = temp_dir.path();
    fs::create_dir_all(root.join("cue.mod"))?;
    fs::write(
        root.join("cue.mod/module.cue"),
        "module: \"example.com/evaluation-options@v0\"\nlanguage: version: \"v0.14.1\"\n",
    )?;
    for (name, contents) in files {
        let path = root.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, format!("package {PACKAGE}\n\n{contents}\n"))?;
    }
    Ok(temp_dir)
}

fn exact_options(root: &Path, concrete_paths: &[&str]) -> ModuleEvalOptions {
    ModuleEvalOptions {
        package_name: Some(PACKAGE.to_string()),
        target_dir: Some(root.to_string_lossy().into_owned()),
        concrete_paths: concrete_paths.iter().map(ToString::to_string).collect(),
        ..Default::default()
    }
}

fn expect_error(result: cuengine::Result<ModuleResult>) -> TestResult<CueEngineError> {
    match result {
        Ok(module) => Err(format!("expected an error, got {:?}", module.instances).into()),
        Err(error) => Ok(error),
    }
}

#[test]
fn nested_concrete_path_validates_only_that_subtree() -> TestResult {
    let module = create_module(&[(
        "values.cue",
        "config: {\n\tstrict: size: 1\n\tlenient: port: int\n}",
    )])?;
    let root = module.path();

    let result = evaluate_module(
        root,
        PACKAGE,
        Some(&exact_options(root, &["config.strict"])),
    )?;
    let instance = result.instances.get(".").ok_or("missing root instance")?;
    assert_eq!(instance["config"]["strict"]["size"], 1);
    assert!(instance["config"]["lenient"]["port"].is_null());

    let error = expect_error(evaluate_module(
        root,
        PACKAGE,
        Some(&exact_options(root, &["config.lenient"])),
    ))?;
    let message = error.to_string();
    assert!(
        message.contains("config.lenient.port") && message.contains("incomplete value int"),
        "unexpected error: {message}"
    );
    Ok(())
}

#[test]
fn missing_concrete_path_fails_closed() -> TestResult {
    let module = create_module(&[("values.cue", "config: size: 1")])?;
    let root = module.path();

    let error = expect_error(evaluate_module(
        root,
        PACKAGE,
        Some(&exact_options(root, &["config.absent"])),
    ))?;
    assert!(
        error
            .to_string()
            .contains("config.absent: concrete path does not exist"),
        "unexpected error: {error}"
    );
    Ok(())
}

#[test]
fn malformed_concrete_path_is_a_configuration_error() -> TestResult {
    let module = create_module(&[("values.cue", "config: size: 1")])?;
    let root = module.path();

    for path in ["", "config..size", "config["] {
        let error = expect_error(evaluate_module(
            root,
            PACKAGE,
            Some(&exact_options(root, &[path])),
        ))?;
        assert!(
            matches!(error, CueEngineError::Configuration { .. }),
            "expected a configuration error for {path:?}, got {error:?}"
        );
    }
    Ok(())
}

#[test]
fn failing_instance_policy_names_every_failed_instance() -> TestResult {
    let module = create_module(&[
        ("valid/values.cue", "config: size: 1"),
        ("conflict/values.cue", "config: size: 1 & 2"),
        ("incomplete/values.cue", "config: size: int"),
    ])?;
    let root = module.path();
    let lenient = ModuleEvalOptions {
        recursive: true,
        package_name: Some(PACKAGE.to_string()),
        concrete_paths: vec!["config".to_string()],
        ..Default::default()
    };

    let result = evaluate_module(root, PACKAGE, Some(&lenient))?;
    assert_eq!(
        result.instances.keys().collect::<Vec<_>>(),
        vec!["valid"],
        "failed instances are skipped by default"
    );

    let strict = ModuleEvalOptions {
        instance_failures: InstanceFailures::Fail,
        ..lenient
    };
    let error = expect_error(evaluate_module(root, PACKAGE, Some(&strict)))?;
    assert!(
        matches!(error, CueEngineError::CueParse { .. }),
        "expected an evaluation error, got {error:?}"
    );
    let message = error.to_string();
    for fragment in [
        "2 instance(s) could not be evaluated",
        "conflict: config.size: conflicting values",
        "incomplete: config: config.size: incomplete value int",
    ] {
        assert!(
            message.contains(fragment),
            "expected {fragment:?} in {message}"
        );
    }
    assert!(
        !message.contains("valid:"),
        "unexpected instance in {message}"
    );
    Ok(())
}
