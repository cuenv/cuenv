//! Tests for task output reference names derived during CUE evaluation.

use cuengine::{ModuleEvalOptions, evaluate_module};
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

fn project_root() -> TestResult<PathBuf> {
    Ok(Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?)
}

fn new_fixture_dir() -> TestResult<TempDir> {
    let fixture_root = project_root()?.join("target/cuengine-test-fixtures");
    fs::create_dir_all(&fixture_root)?;
    Ok(tempfile::Builder::new()
        .prefix("task-output-name-")
        .tempdir_in(fixture_root)?)
}

fn evaluate_fixture(target_dir: &Path) -> TestResult<cuengine::ModuleResult> {
    evaluate_fixture_with_task_field(target_dir, None)
}

fn evaluate_fixture_with_task_field(
    target_dir: &Path,
    task_field: Option<&str>,
) -> TestResult<cuengine::ModuleResult> {
    let options = ModuleEvalOptions {
        recursive: false,
        target_dir: Some(target_dir.display().to_string()),
        task_field: task_field.map(str::to_owned),
        ..Default::default()
    };

    Ok(evaluate_module(
        &project_root()?,
        "fixture",
        Some(&options),
    )?)
}

#[test]
fn derives_output_ref_names_for_hyphenated_named_tasks() -> TestResult {
    let temp = new_fixture_dir()?;
    let fixture_dir = temp.path();

    fs::write(
        fixture_dir.join("env.cue"),
        r#"package fixture

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "task-output-name-test"

tasks: {
    "sync-check": schema.#Task & {
        command: "echo"
        args: ["-n", "sync"]
    }
    consumeSync: schema.#Task & {
        command: "echo"
        args: [tasks."sync-check".stdout]
    }
    checks: schema.#TaskGroup & {
        type: "group"
        "fmt-check": schema.#Task & {
            command: "echo"
            args: ["-n", "fmt"]
        }
        consumeFmt: schema.#Task & {
            command: "echo"
            args: [tasks.checks."fmt-check".stdout]
        }
    }
}
"#,
    )?;

    let result = evaluate_fixture(fixture_dir)?;
    assert_eq!(
        result.instances.len(),
        1,
        "expected exactly one fixture instance"
    );
    let instance = result
        .instances
        .values()
        .next()
        .ok_or_else(|| std::io::Error::other("fixture instance missing"))?;

    assert_eq!(
        instance["tasks"]["sync-check"]["stdout"]["cuenvTask"].as_str(),
        Some("sync-check")
    );
    assert_eq!(
        instance["tasks"]["consumeSync"]["args"][0]["cuenvTask"].as_str(),
        Some("sync-check")
    );
    assert_eq!(
        instance["tasks"]["checks"]["fmt-check"]["stdout"]["cuenvTask"].as_str(),
        Some("checks.fmt-check")
    );
    assert_eq!(
        instance["tasks"]["checks"]["consumeFmt"]["args"][0]["cuenvTask"].as_str(),
        Some("checks.fmt-check")
    );

    Ok(())
}

#[test]
fn fills_sequence_item_names_under_hyphenated_parents() -> TestResult {
    let temp = new_fixture_dir()?;
    let fixture_dir = temp.path();

    fs::write(
        fixture_dir.join("env.cue"),
        r#"package fixture

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "task-sequence-name-test"

tasks: {
    "release-check": schema.#TaskSequence & [
        schema.#Task & {
            command: "echo"
            args: ["-n", "first"]
        },
        schema.#Task & {
            command: "echo"
            args: ["received:", tasks."release-check"[0].stdout]
        },
        schema.#TaskGroup & {
            type: "group"
            verify: schema.#Task & {
                command: "echo"
                args: [tasks."release-check"[0].stdout]
            }
        },
    ]
}
"#,
    )?;

    let result = evaluate_fixture(fixture_dir)?;
    assert_eq!(
        result.instances.len(),
        1,
        "expected exactly one fixture instance"
    );
    let instance = result
        .instances
        .values()
        .next()
        .ok_or_else(|| std::io::Error::other("fixture instance missing"))?;

    assert_eq!(
        instance["tasks"]["release-check"][0]["stdout"]["cuenvTask"].as_str(),
        Some("release-check[0]")
    );
    assert_eq!(
        instance["tasks"]["release-check"][1]["args"][1]["cuenvTask"].as_str(),
        Some("release-check[0]")
    );
    assert_eq!(
        instance["tasks"]["release-check"][2]["verify"]["stdout"]["cuenvTask"].as_str(),
        Some("release-check[2].verify")
    );
    assert_eq!(
        instance["tasks"]["release-check"][2]["verify"]["args"][0]["cuenvTask"].as_str(),
        Some("release-check[0]")
    );

    Ok(())
}

const SEQUENCE_FIXTURE: &str = r#"package fixture

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "task-field-test"

tasks: {
    "release-check": schema.#TaskSequence & [
        schema.#Task & {
            command: "echo"
            args: ["-n", "first"]
        },
        schema.#Task & {
            command: "echo"
            args: ["received:", tasks."release-check"[0].stdout]
        },
    ]
}
"#;

fn sequence_item_name(task_field: Option<&str>) -> TestResult<Option<String>> {
    let temp = new_fixture_dir()?;
    fs::write(temp.path().join("env.cue"), SEQUENCE_FIXTURE)?;
    let result = evaluate_fixture_with_task_field(temp.path(), task_field)?;
    let instance = result
        .instances
        .values()
        .next()
        .ok_or_else(|| std::io::Error::other("fixture instance missing"))?;
    Ok(
        instance["tasks"]["release-check"][1]["args"][1]["cuenvTask"]
            .as_str()
            .map(str::to_owned),
    )
}

#[test]
fn task_field_defaults_to_the_field_callers_always_used() -> TestResult {
    let expected = Some("release-check[0]".to_string());
    assert_eq!(sequence_item_name(None)?, expected);
    assert_eq!(sequence_item_name(Some("tasks"))?, expected);
    Ok(())
}

#[test]
fn task_field_names_the_field_whose_sequence_items_are_named() -> TestResult {
    // The injection follows the caller-supplied field: naming another field,
    // or none, leaves the sequence items of `tasks` unnamed.
    let named = Some("release-check[0]".to_string());
    assert_ne!(sequence_item_name(Some("pipeline"))?, named);
    assert_ne!(sequence_item_name(Some(""))?, named);
    Ok(())
}
