//! Tests for the `concrete_paths`, `instance_failures` and `package_scope`
//! evaluation options across the FFI boundary.

use cuengine::{
    CueEngineError, InstanceFailures, ModuleEvalOptions, ModuleResult, PackageScope,
    SkippedDirectories, SkippedDirectory, SkippedReason, evaluate_module,
};
use std::error::Error;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

const PACKAGE: &str = "app";

/// Creates a module; `files` maps a relative path to the package body.
fn create_module(files: &[(&str, &str)]) -> TestResult<TempDir> {
    let sources: Vec<(&str, String)> = files
        .iter()
        .map(|(name, contents)| (*name, format!("package {PACKAGE}\n\n{contents}\n")))
        .collect();
    create_module_from_sources(&sources)
}

/// Creates a module; `files` maps a relative path to its complete source.
fn create_module_from_sources(files: &[(&str, String)]) -> TestResult<TempDir> {
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
        fs::write(path, contents)?;
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

/// A module with the test package at the root, a directory holding two
/// packages and, when `broken` is given, a failing third package there.
fn create_multiple_package_module(broken: Option<&str>) -> TestResult<TempDir> {
    let mut files = vec![
        (
            "values.cue",
            format!("package {PACKAGE}\n\nname: \"root\"\n"),
        ),
        (
            "mixed/alpha.cue",
            "package alpha\n\nname: \"alpha\"\n".to_string(),
        ),
        (
            "mixed/beta.cue",
            "package beta\n\nname: \"beta\"\n".to_string(),
        ),
    ];
    if let Some(source) = broken {
        files.push(("mixed/broken.cue", source.to_string()));
    }
    create_module_from_sources(&files)
}

fn all_packages() -> ModuleEvalOptions {
    ModuleEvalOptions {
        recursive: true,
        package_scope: PackageScope::All,
        instance_failures: InstanceFailures::Fail,
        ..Default::default()
    }
}

#[test]
fn all_packages_keys_instances_by_directory_and_package() -> TestResult {
    let module = create_multiple_package_module(None)?;
    let result = evaluate_module(module.path(), "", Some(&all_packages()))?;

    let mut keys: Vec<&str> = result.instances.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, vec![".:app", "mixed:alpha", "mixed:beta"]);
    assert_eq!(result.instances["mixed:alpha"]["name"], "alpha");
    assert_eq!(result.instances["mixed:beta"]["name"], "beta");

    let mut projects = result.projects.clone();
    projects.sort_unstable();
    assert_eq!(projects, vec![".:app", "mixed:alpha", "mixed:beta"]);
    Ok(())
}

#[test]
fn all_packages_with_failing_policy_fails_on_any_package() -> TestResult {
    let module = create_multiple_package_module(Some("package broken\n\nname: 1 & 2\n"))?;
    let root = module.path();

    let lenient = ModuleEvalOptions {
        instance_failures: InstanceFailures::Skip,
        ..all_packages()
    };
    let result = evaluate_module(root, "", Some(&lenient))?;
    assert!(!result.instances.contains_key("mixed:broken"));
    assert_eq!(result.instances.len(), 3);

    let error = expect_error(evaluate_module(root, "", Some(&all_packages())))?;
    let message = error.to_string();
    assert!(
        message.contains("1 instance(s) could not be evaluated")
            && message.contains("mixed:broken: name: conflicting values"),
        "unexpected error: {message}"
    );
    Ok(())
}

#[test]
fn all_packages_rejects_a_package_name() -> TestResult {
    let module = create_multiple_package_module(None)?;
    let root = module.path();

    let error = expect_error(evaluate_module(root, PACKAGE, Some(&all_packages())))?;
    assert!(
        matches!(error, CueEngineError::Configuration { .. }),
        "expected a configuration error, got {error:?}"
    );

    let named = ModuleEvalOptions {
        package_name: Some(PACKAGE.to_string()),
        ..all_packages()
    };
    let error = expect_error(evaluate_module(root, "", Some(&named)))?;
    assert!(
        matches!(error, CueEngineError::Configuration { .. }),
        "expected a configuration error, got {error:?}"
    );
    Ok(())
}

#[test]
fn named_scope_keeps_directory_keys() -> TestResult {
    let module = create_multiple_package_module(None)?;
    let options = ModuleEvalOptions {
        recursive: true,
        package_name: Some(PACKAGE.to_string()),
        ..Default::default()
    };
    let result = evaluate_module(module.path(), PACKAGE, Some(&options))?;
    assert_eq!(result.instances.keys().collect::<Vec<_>>(), vec!["."]);
    Ok(())
}

#[test]
fn export_paths_and_presence_paths_project_every_instance() -> TestResult {
    let module = create_module(&[
        (
            "a/values.cue",
            "name: \"a\"\ninfrastructure: url: \"libsql://a\"\ntasks: build: command: \"make\"",
        ),
        ("b/values.cue", "name: \"b\""),
    ])?;
    let options = ModuleEvalOptions {
        export_paths: vec!["name".to_string()],
        presence_paths: vec!["infrastructure".to_string()],
        ..all_packages()
    };
    let result = evaluate_module(module.path(), "", Some(&options))?;

    assert_eq!(result.instances["a:app"], serde_json::json!({"name": "a"}));
    assert_eq!(result.instances["b:app"], serde_json::json!({"name": "b"}));
    assert_eq!(result.present["a:app"], vec!["infrastructure".to_string()]);
    assert!(result.present["b:app"].is_empty());
    Ok(())
}

#[test]
fn invalid_export_path_is_a_configuration_error() -> TestResult {
    let module = create_multiple_package_module(None)?;
    let options = ModuleEvalOptions {
        export_paths: vec!["tasks[0]".to_string()],
        ..all_packages()
    };
    let error = expect_error(evaluate_module(module.path(), "", Some(&options)))?;
    assert!(
        matches!(error, CueEngineError::Configuration { .. }),
        "expected a configuration error, got {error:?}"
    );
    Ok(())
}

#[test]
fn skipped_directories_are_reported_on_request() -> TestResult {
    let module = create_module(&[
        ("values.cue", "name: \"root\""),
        ("_drafts/values.cue", "name: \"draft\""),
        ("tools/testdata/values.cue", "name: \"fixture\""),
    ])?;
    let report = ModuleEvalOptions {
        skipped_directories: SkippedDirectories::Report,
        ..all_packages()
    };
    let result = evaluate_module(module.path(), "", Some(&report))?;
    assert_eq!(result.instances.keys().collect::<Vec<_>>(), vec![".:app"]);
    assert_eq!(
        result.skipped_directories,
        vec![
            SkippedDirectory {
                path: "_drafts".to_string(),
                reason: SkippedReason::Underscore,
            },
            SkippedDirectory {
                path: "tools/testdata".to_string(),
                reason: SkippedReason::Testdata,
            },
        ]
    );

    let silent = evaluate_module(module.path(), "", Some(&all_packages()))?;
    assert!(silent.skipped_directories.is_empty());
    Ok(())
}

#[test]
fn module_root_named_like_a_skipped_directory_is_evaluated() -> TestResult {
    for name in [".hidden-module", "_module"] {
        let parent = tempfile::Builder::new()
            .prefix("cuengine-root-name-")
            .tempdir()?;
        let root = parent.path().join(name);
        fs::create_dir_all(root.join("cue.mod"))?;
        fs::create_dir_all(root.join("child"))?;
        fs::write(
            root.join("cue.mod/module.cue"),
            "module: \"example.com/root-name@v0\"\nlanguage: version: \"v0.14.1\"\n",
        )?;
        fs::write(
            root.join("values.cue"),
            format!("package {PACKAGE}\n\nkind: \"root\"\n"),
        )?;
        fs::write(
            root.join("child/values.cue"),
            format!("package {PACKAGE}\n\nchild: true\n"),
        )?;

        let result = evaluate_module(&root, "", Some(&all_packages()))?;
        let mut keys: Vec<&str> = result.instances.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, vec![".:app", "child:app"], "root {name}");
    }
    Ok(())
}

#[test]
fn evaluation_errors_list_every_error_without_option_hints() -> TestResult {
    let module = create_module(&[(
        "broken/values.cue",
        "#Config: close({size: int, name: string})\nconfig: #Config & {sise: 1, nmae: \"x\"}",
    )])?;
    let error = expect_error(evaluate_module(module.path(), "", Some(&all_packages())))?;
    let message = error.to_string();
    assert!(
        message.starts_with("CUE evaluation failed at "),
        "unexpected error: {message}"
    );
    for fragment in [
        "config.sise: field not allowed",
        "config.nmae: field not allowed",
        "./broken/values.cue:4:",
    ] {
        assert!(message.contains(fragment), "{fragment} missing: {message}");
    }
    for leaked in ["Hint", "instanceFailures", "evalDir="] {
        assert!(!message.contains(leaked), "{leaked} leaked: {message}");
    }
    Ok(())
}
