//! Tests for reference extraction in module evaluation.

use cuengine::{ModuleEvalOptions, evaluate_module};
use std::fs;
use std::io;
use std::path::Path;
use tempfile::TempDir;

fn init_module(module_root: &Path, module_name: &str) -> io::Result<()> {
    fs::create_dir_all(module_root.join("cue.mod"))?;
    fs::write(
        module_root.join("cue.mod/module.cue"),
        format!("module: \"{module_name}\"\nlanguage: {{ version: \"v0.14.1\" }}\n"),
    )?;
    Ok(())
}

fn strip_task_ref_prefix(reference: &str) -> &str {
    for prefix in ["tasks.", "_tasks.", "_t."] {
        if let Some(stripped) = reference.strip_prefix(prefix) {
            return stripped;
        }
    }
    reference
}

#[test]
fn extracts_depends_on_reference_inside_selector_wrapped_expression() {
    let temp = TempDir::new().expect("failed to create temp dir");
    let module_root = temp.path();

    let init = init_module(module_root, "example.com/ref-test");
    assert!(
        init.is_ok(),
        "failed to initialize module fixture: {:?}",
        init.err()
    );

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
fn extracts_task_node_references_for_depends_on_and_ci_paths() {
    let temp = TempDir::new().expect("failed to create temp dir");
    let module_root = temp.path();
    let init = init_module(module_root, "example.com/task-node-ref-test");
    assert!(
        init.is_ok(),
        "failed to initialize module fixture: {:?}",
        init.err()
    );

    fs::write(
        module_root.join("env.cue"),
        r#"package cuenv

{
    name: "shape-test"
    tasks: {
        leaf: {
            command: "echo"
            args: ["leaf"]
        }
        group: {
            type: "group"
            step: {
                command: "echo"
                args: ["group"]
            }
        }
        sequence: [
            {
                command: "echo"
                args: ["seq-0"]
            },
            {
                command: "echo"
                args: ["seq-1"]
            },
        ]

        depTaskLeaf: {
            command: "echo"
            dependsOn: [leaf]
        }
        depTaskGroup: {
            command: "echo"
            dependsOn: [group]
        }
        depTaskSequence: {
            command: "echo"
            dependsOn: [sequence]
        }

        depGroupLeaf: {
            type: "group"
            dependsOn: [leaf]
            step: {
                command: "echo"
                args: ["run"]
            }
        }
        depGroupGroup: {
            type: "group"
            dependsOn: [group]
            step: {
                command: "echo"
                args: ["run"]
            }
        }
        depGroupSequence: {
            type: "group"
            dependsOn: [sequence]
            step: {
                command: "echo"
                args: ["run"]
            }
        }
    }
    _t: tasks
    ci: {
        pipelines: {
            default: {
                tasks: [
                    _t.leaf,
                    _t.group,
                    _t.sequence,
                    {
                        type: "matrix"
                        task: _t.leaf
                        matrix: {
                            arch: ["linux-x64"]
                        }
                    },
                    {
                        type: "matrix"
                        task: _t.group
                        matrix: {
                            arch: ["linux-x64"]
                        }
                    },
                    {
                        type: "matrix"
                        task: _t.sequence
                        matrix: {
                            arch: ["linux-x64"]
                        }
                    },
                ]
            }
        }
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

    for (meta_key, expected_task_name) in [
        ("./tasks.depTaskLeaf.dependsOn[0]", "leaf"),
        ("./tasks.depTaskGroup.dependsOn[0]", "group"),
        ("./tasks.depTaskSequence.dependsOn[0]", "sequence"),
        ("./tasks.depGroupLeaf.dependsOn[0]", "leaf"),
        ("./tasks.depGroupGroup.dependsOn[0]", "group"),
        ("./tasks.depGroupSequence.dependsOn[0]", "sequence"),
        ("./ci.pipelines.default.tasks[0]", "leaf"),
        ("./ci.pipelines.default.tasks[1]", "group"),
        ("./ci.pipelines.default.tasks[2]", "sequence"),
        ("./ci.pipelines.default.tasks[3].task", "leaf"),
        ("./ci.pipelines.default.tasks[4].task", "group"),
        ("./ci.pipelines.default.tasks[5].task", "sequence"),
    ] {
        let reference = result
            .meta
            .get(meta_key)
            .and_then(|meta| meta.reference.as_deref());
        let normalized = reference.map(strip_task_ref_prefix);
        assert_eq!(
            normalized,
            Some(expected_task_name),
            "reference mismatch for {meta_key}"
        );
    }
}

#[test]
fn preserves_empty_lists_as_json_arrays() {
    let temp = TempDir::new().expect("failed to create temp dir");
    let module_root = temp.path();

    let init = init_module(module_root, "example.com/list-test");
    assert!(
        init.is_ok(),
        "failed to initialize module fixture: {:?}",
        init.err()
    );

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
