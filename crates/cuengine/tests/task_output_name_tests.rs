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
    let options = ModuleEvalOptions {
        recursive: false,
        target_dir: Some(target_dir.display().to_string()),
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
