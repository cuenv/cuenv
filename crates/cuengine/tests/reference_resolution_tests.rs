//! Tests for reference extraction in module evaluation.

use cuengine::{ModuleEvalOptions, evaluate_module};
use std::fs;
use tempfile::TempDir;

#[test]
fn extracts_depends_on_reference_inside_selector_wrapped_expression() {
    let temp = TempDir::new().expect("failed to create temp dir");
    let module_root = temp.path();

    fs::create_dir_all(module_root.join("cue.mod")).expect("failed to create cue.mod");
    fs::write(
        module_root.join("cue.mod/module.cue"),
        "module: \"example.com/ref-test\"\nlanguage: { version: \"v0.14.1\" }\n",
    )
    .expect("failed to write module.cue");

    fs::write(
        module_root.join("env.cue"),
        r#"package cuenv

#PublishModule: {
    DO = dependsOn: [...]
    output: {
        command: "bash"
        dependsOn: DO
    }
}

{
    name: "ref-test"
    tasks: {
        build: {
            command: "echo"
            args: ["build"]
        }
        publish: (#PublishModule & {dependsOn: [build]}).output
    }
}
"#,
    )
    .expect("failed to write env.cue");

    let options = ModuleEvalOptions {
        recursive: false,
        with_meta: true,
        with_references: true,
        target_dir: Some(module_root.display().to_string()),
        ..Default::default()
    };

    let result = evaluate_module(module_root, "cuenv", Some(&options))
        .expect("module evaluation should succeed");

    let reference = result
        .meta
        .get("./tasks.publish.dependsOn[0]")
        .and_then(|meta| meta.reference.as_deref());

    assert_eq!(
        reference,
        Some("build"),
        "dependsOn reference should resolve to the task identifier"
    );
}

#[test]
fn preserves_empty_lists_as_json_arrays() {
    let temp = TempDir::new().expect("failed to create temp dir");
    let module_root = temp.path();

    fs::create_dir_all(module_root.join("cue.mod")).expect("failed to create cue.mod");
    fs::write(
        module_root.join("cue.mod/module.cue"),
        "module: \"example.com/list-test\"\nlanguage: { version: \"v0.14.1\" }\n",
    )
    .expect("failed to write module.cue");

    fs::write(
        module_root.join("env.cue"),
        r#"package cuenv

{
    name: "list-test"
    tasks: {
        build: {
            command: "echo"
            args: ["ok"]
            dependsOn: []
        }
    }
}
"#,
    )
    .expect("failed to write env.cue");

    let options = ModuleEvalOptions {
        recursive: false,
        with_meta: false,
        with_references: false,
        target_dir: Some(module_root.display().to_string()),
        ..Default::default()
    };

    let result = evaluate_module(module_root, "cuenv", Some(&options))
        .expect("module evaluation should succeed");

    let depends_on = &result.instances["."]["tasks"]["build"]["dependsOn"];
    assert_eq!(
        depends_on,
        &serde_json::json!([]),
        "empty list should serialize as [] instead of null"
    );
}
