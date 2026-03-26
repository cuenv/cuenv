//! Tests for task output reference names derived during CUE evaluation.

#![allow(clippy::expect_used)]

use cuengine::{ModuleEvalOptions, evaluate_module};
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

fn project_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("failed to derive project root")
        .to_path_buf()
}

fn new_fixture_dir() -> TempDir {
    let fixture_root = project_root().join("target/cuengine-test-fixtures");
    fs::create_dir_all(&fixture_root).expect("failed to create fixture root");
    tempfile::Builder::new()
        .prefix("task-output-name-")
        .tempdir_in(fixture_root)
        .expect("failed to create fixture dir")
}

fn evaluate_fixture(target_dir: &Path) -> cuengine::Result<cuengine::ModuleResult> {
    let options = ModuleEvalOptions {
        recursive: false,
        target_dir: Some(target_dir.display().to_string()),
        ..Default::default()
    };

    evaluate_module(&project_root(), "fixture", Some(&options))
}

#[test]
fn derives_output_ref_names_for_hyphenated_named_tasks() {
    let temp = new_fixture_dir();
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
    )
    .expect("failed to write env.cue");

    let result = evaluate_fixture(fixture_dir).expect("module evaluation should succeed");
    assert_eq!(
        result.instances.len(),
        1,
        "expected exactly one fixture instance"
    );
    let instance = result
        .instances
        .values()
        .next()
        .expect("fixture instance missing");

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
}

#[test]
fn fills_sequence_item_names_under_hyphenated_parents() {
    let temp = new_fixture_dir();
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
    )
    .expect("failed to write env.cue");

    let result = evaluate_fixture(fixture_dir).expect("module evaluation should succeed");
    assert_eq!(
        result.instances.len(),
        1,
        "expected exactly one fixture instance"
    );
    let instance = result
        .instances
        .values()
        .next()
        .expect("fixture instance missing");

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
}
