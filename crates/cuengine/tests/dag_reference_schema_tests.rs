//! Schema-backed regression coverage for shallow DAG reference validation.

use cuengine::{ModuleEvalOptions, ModuleResult, evaluate_module};
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

fn assert_evaluation_fails_with(source: &str, label: &str, expected: &[&str]) -> TestResult {
    let message = evaluation_failure_message(source, label)?;
    if !expected.iter().any(|fragment| message.contains(fragment)) {
        return Err(std::io::Error::other(format!(
            "{label} failed for an unexpected reason; expected one of {expected:?}: {message}"
        ))
        .into());
    }

    Ok(())
}

fn evaluation_failure_message(source: &str, label: &str) -> TestResult<String> {
    let message = match evaluate_source(source) {
        Ok(result) => {
            return Err(std::io::Error::other(format!(
                "{label} unexpectedly passed: {:?}",
                result.instances
            ))
            .into());
        }
        Err(error) => error.to_string(),
    };
    let lowercase = message.to_lowercase();
    for infrastructure_error in [
        "internal panic",
        "timed out",
        "timeout after",
        "failed to initialize cue registry",
        "network is unreachable",
        "proxy.golang.org",
        "no cue instances found",
    ] {
        if lowercase.contains(infrastructure_error) {
            return Err(std::io::Error::other(format!(
                "{label} failed because of infrastructure, not validation: {message}"
            ))
            .into());
        }
    }
    Ok(message)
}

fn project_root() -> TestResult<PathBuf> {
    Ok(Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?)
}

fn new_fixture_dir() -> TestResult<TempDir> {
    let fixture_root = project_root()?.join("target/cuengine-test-fixtures");
    fs::create_dir_all(&fixture_root)?;
    Ok(tempfile::Builder::new()
        .prefix("dag-reference-schema-")
        .tempdir_in(fixture_root)?)
}

fn evaluate_source(source: &str) -> TestResult<ModuleResult> {
    let temp = new_fixture_dir()?;
    let fixture_dir = temp.path();
    fs::write(fixture_dir.join("env.cue"), source)?;

    let options = ModuleEvalOptions {
        recursive: false,
        with_meta: true,
        with_references: true,
        target_dir: Some(fixture_dir.display().to_string()),
        ..Default::default()
    };

    Ok(evaluate_module(
        &project_root()?,
        "fixture",
        Some(&options),
    )?)
}

#[test]
fn schema_dependencies_accept_all_validated_reference_shapes() -> TestResult {
    let result = evaluate_source(
        r#"package fixture

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "dag-reference-schema-test"

tasks: {
    build: schema.#Task & {
        command: "echo"
        args: ["build"]
    }
    checks: schema.#TaskGroup & {
        type: "group"
        lint: schema.#Task & {command: "echo"}
    }
    sequence: schema.#TaskSequence & [
        schema.#Task & {
            command: "echo"
            args: [tasks.build.stdout]
        },
        schema.#TaskGroup & {
            type: "group"
            verify: schema.#Task & {command: "echo"}
        },
    ]
    taskConsumer: schema.#Task & {
        command: "echo"
        dependsOn: [build, checks, sequence, sequence[0], sequence[1]]
    }
    groupConsumer: schema.#TaskGroup & {
        type: "group"
        dependsOn: [build, checks, sequence[0], sequence[1]]
        run: schema.#Task & {command: "echo"}
    }
}

_t: tasks

images: {
    base: schema.#ContainerImage & {context: "."}
    app: schema.#ContainerImage & {
        context: "."
        dependsOn: [tasks.build, tasks.checks, tasks.sequence[0], base]
    }
}

services: {
    db: schema.#Service & {
        entrypoint: schema.#Command & {command: "echo"}
    }
    api: schema.#Service & {
        entrypoint: schema.#Command & {command: "echo"}
        dependsOn: [tasks.build, tasks.checks, tasks.sequence[0], images.base, db]
    }
}

ci: {
    pipelines: {
        default: {
            tasks: [
                _t.build,
                _t.checks,
                _t.sequence[0],
                {type: "matrix", task: _t.sequence[0], matrix: {arch: ["linux-x64"]}},
                schema.#Task & {command: "echo", args: ["inline"]},
            ]
        }
    }
}
"#,
    )?;

    let project = result
        .instances
        .values()
        .next()
        .ok_or("expected the root project instance")?;

    assert_eq!(
        project["tasks"]["sequence"][0]["args"][0]["cuenvTask"].as_str(),
        Some("build"),
        "sequence output refs should retain their source task name"
    );
    assert_eq!(
        project["ci"]["pipelines"]["default"]["tasks"][4]["dir"]["from"].as_str(),
        Some("definition"),
        "inline pipeline tasks should retain schema defaults"
    );
    assert_eq!(
        project["ci"]["pipelines"]["default"]["tasks"][3]["type"].as_str(),
        Some("matrix"),
        "the matrix schema should resolve the discriminator for matrix-shaped entries"
    );
    assert!(
        project["tasks"]["build"]
            .get("_cuenvValidatedDAGNode")
            .is_none(),
        "the internal validation marker must not leak into project JSON"
    );

    for (meta_path, expected) in [
        ("./tasks.taskConsumer.dependsOn[0]", "build"),
        ("./tasks.taskConsumer.dependsOn[3]", "sequence"),
        ("./images.app.dependsOn[2]", "sequence"),
        ("./services.api.dependsOn[2]", "sequence"),
        ("./ci.pipelines.default.tasks[2]", "sequence"),
        ("./ci.pipelines.default.tasks[3].task", "sequence"),
    ] {
        let actual = result
            .meta
            .iter()
            .find(|(path, _)| path.ends_with(meta_path.trim_start_matches("./")))
            .map(|(_, meta)| meta)
            .and_then(|meta| meta.reference.as_deref());
        assert_eq!(
            actual.map(strip_task_reference_prefix),
            Some(expected),
            "reference metadata mismatch at {meta_path}"
        );
    }

    Ok(())
}

#[test]
fn schema_dependencies_accept_a_refined_task_alias() -> TestResult {
    evaluate_source(
        r#"package fixture

import "github.com/cuenv/cuenv/schema"

schema.#Project
name: "dag-refined-reference-test"
tasks: {
    build: schema.#Task & {command: "echo"}
    consumer: schema.#Task & {
        command: "echo"
        dependsOn: [build & {description: "valid alias"}]
    }
}
"#,
    )?;

    Ok(())
}

#[test]
fn schema_dependency_markers_survive_reusable_task_aliases() -> TestResult {
    let result = evaluate_source(
        r#"package fixture

import "github.com/cuenv/cuenv/schema"

#Reusable: {
    tasks: {
        compile: schema.#Task & {command: "echo"}
        release: schema.#Task & {
            command: "echo"
            dependsOn: [compile]
        }
    }
}

_reusable: #Reusable

schema.#Project
name: "dag-reference-reusable-test"
tasks: _reusable.tasks
"#,
    )?;

    let dependency_reference = result
        .meta
        .iter()
        .find(|(path, meta)| {
            path.ends_with("dependsOn[0]") && meta.reference.as_deref() == Some("compile")
        })
        .and_then(|(_, meta)| meta.reference.as_deref());
    assert_eq!(
        dependency_reference.map(strip_task_reference_prefix),
        Some("compile"),
        "reusable CUE references should keep canonical task identity"
    );

    Ok(())
}

#[test]
fn schema_dependency_target_kinds_remain_restricted() -> TestResult {
    let cases = [
        (
            "task to service",
            "dependsOn: [services.db]",
            "",
            "",
            "tasks.taskConsumer",
        ),
        (
            "group to image",
            "",
            "dependsOn: [images.base]",
            "",
            "tasks.groupConsumer",
        ),
        (
            "image to service",
            "",
            "",
            "dependsOn: [services.db]",
            "images.app",
        ),
    ];

    for (label, task_dependency, group_dependency, image_dependency, expected_path) in cases {
        let source = format!(
            r#"package fixture

import "github.com/cuenv/cuenv/schema"

schema.#Project
name: "dag-reference-kind-test"

tasks: {{
    build: schema.#Task & {{command: "echo"}}
    taskConsumer: schema.#Task & {{command: "echo", {task_dependency}}}
    groupConsumer: schema.#TaskGroup & {{
        type: "group"
        {group_dependency}
        run: schema.#Task & {{command: "echo"}}
    }}
}}

images: {{
    base: schema.#ContainerImage & {{context: "."}}
    app: schema.#ContainerImage & {{context: ".", {image_dependency}}}
}}

services: {{
    db: schema.#Service & {{entrypoint: schema.#Command & {{command: "echo"}}}}
}}
"#
        );

        assert_evaluation_fails_with(&source, label, &[expected_path])?;
    }

    Ok(())
}

#[test]
fn pipeline_only_projects_validate_named_and_inline_nodes() -> TestResult {
    let valid = evaluate_source(
        r#"package fixture

import "github.com/cuenv/cuenv/schema"

schema.#Project
name: "pipeline-only-valid"
tasks: {
    sequence: schema.#TaskSequence & [
        schema.#Task & {command: "echo", args: ["named sequence"]},
    ]
}
_t: tasks
ci: {
    pipelines: {
        default: {
            tasks: [
                {command: "echo", args: ["inline"]},
                {
                    type: "group"
                    build: {command: "echo"}
                },
                {type: "matrix", task: schema.#Task & {command: "echo"}, matrix: {arch: ["linux-x64"]}},
                {type: "group", task: {command: "echo"}},
                {type: "group", matrix: {command: "echo"}},
                {
                    type: "group"
                    task: {command: "echo"}
                    matrix: {command: "echo"}
                },
                [{command: "echo", args: ["inline sequence"]}],
                _t.sequence,
            ]
        }
    }
}
"#,
    )?;
    let project = valid
        .instances
        .values()
        .next()
        .ok_or("expected the pipeline-only project instance")?;
    assert_eq!(
        project["ci"]["pipelines"]["default"]["tasks"][0]["dir"]["from"].as_str(),
        Some("definition"),
        "inline task defaults should survive the pipeline fast path"
    );
    assert_eq!(
        project["ci"]["pipelines"]["default"]["tasks"][1]["type"].as_str(),
        Some("group"),
        "inline task groups should survive the pipeline fast path"
    );
    assert_eq!(
        project["ci"]["pipelines"]["default"]["tasks"][2]["type"].as_str(),
        Some("matrix"),
        "matrix-shaped pipeline entries should keep the matrix discriminator"
    );
    assert_eq!(
        project["ci"]["pipelines"]["default"]["tasks"][5]["task"]["command"].as_str(),
        Some("echo"),
        "groups may use both task and matrix as child labels"
    );
    assert_eq!(
        project["ci"]["pipelines"]["default"]["tasks"][6][0]["args"][0].as_str(),
        Some("inline sequence"),
        "inline pipeline sequences should remain supported"
    );
    assert_eq!(
        project["ci"]["pipelines"]["default"]["tasks"][7][0]["args"][0].as_str(),
        Some("named sequence"),
        "named pipeline sequences should remain supported"
    );

    for (label, task, expected_path) in [
        (
            "malformed inline task",
            "{command: 123}",
            "ci.pipelines.default.tasks.0",
        ),
        (
            "malformed inline group",
            "{type: \"group\", invalidChild: 123}",
            "ci.pipelines.default.tasks.0",
        ),
        (
            "malformed matrix task",
            "{type: \"matrix\", task: {command: 123}, matrix: {arch: [\"linux-x64\"]}}",
            "ci.pipelines.default.tasks.0",
        ),
        (
            "pipeline task output reference",
            "{command: \"echo\", dependsOn: [tasks.build.stdout]}",
            "ci.pipelines.default.tasks.0",
        ),
    ] {
        let source = format!(
            r#"package fixture

import "github.com/cuenv/cuenv/schema"

schema.#Project
name: "pipeline-only-invalid"
tasks: {{
    build: schema.#Task & {{command: "echo"}}
}}
ci: {{
    pipelines: {{
        default: {{
            tasks: [{task}]
        }}
    }}
}}
"#
        );
        assert_evaluation_fails_with(&source, label, &[expected_path])?;
    }

    Ok(())
}

#[test]
fn schema_dependencies_reject_unvalidated_or_tampered_values() -> TestResult {
    for (label, dependency) in [
        ("task output reference", "build.stdout"),
        ("anonymous dependency object", "{command: 123}"),
        (
            "publicly forged identity and marker",
            "{\"_name\": \"build\", \"_cuenvValidatedDAGNode\": \"task\"}",
        ),
        ("unvalidated hidden definition", "#Unvalidated"),
        ("conflicting referenced field", "build & {command: 123}"),
        (
            "referenced node refined with an invalid edge",
            "build & {dependsOn: [build.stdout]}",
        ),
        ("unknown referenced field", "build & {notATaskField: true}"),
    ] {
        let source = format!(
            r#"package fixture

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "dag-reference-schema-negative-test"

#Unvalidated: {{command: 123}}

tasks: {{
    build: schema.#Task & {{command: "echo"}}
    consumer: schema.#Task & {{command: "echo", dependsOn: [{dependency}]}}
}}
"#
        );

        assert_evaluation_fails_with(&source, label, &["dependsOn[0]", "tasks.consumer"])?;
    }

    let source = r#"package fixture

import "github.com/cuenv/cuenv/schema"

schema.#Project
name: "dag-differently-refined-aliases-test"
tasks: {
    build: schema.#Task & {command: "echo"}
    consumer: schema.#Task & {
        command: "echo"
        dependsOn: [
            build & {description: "valid alias"},
            build & {dependsOn: [build.stdout]},
        ]
    }
}
"#;

    assert_evaluation_fails_with(
        source,
        "two differently refined aliases of one task",
        &["tasks.consumer.dependsOn[1].dependsOn[0]"],
    )?;

    Ok(())
}

#[test]
fn referenced_schema_projects_validate_nested_task_edges() -> TestResult {
    let source = r#"package fixture

import "github.com/cuenv/cuenv/schema"

#Other: schema.#Project & {
    name: "other"
    tasks: {
        build: schema.#Task & {command: "echo"}
        invalid: schema.#Task & {command: "echo", dependsOn: [build.stdout]}
    }
}

schema.#Project
name: "root"
tasks: {
    consumer: schema.#Task & {command: "echo", dependsOn: [#Other.tasks.invalid]}
}
"#;

    assert_evaluation_fails_with(
        source,
        "referenced task from another schema project",
        &["tasks.consumer.dependsOn[0].dependsOn[0]"],
    )?;

    Ok(())
}

#[test]
fn path_separator_labels_do_not_collide_in_dag_validation() -> TestResult {
    let cases = [
        (
            "dotted root label and group child",
            r#"tasks: {
    "group.child": schema.#Task & {command: "echo"}
    group: schema.#TaskGroup & {
        type: "group"
        child: schema.#Task & {command: "echo", dependsOn: [tasks["group.child"].stdout]}
    }
}"#,
            "tasks.group.child.dependsOn[0]",
        ),
        (
            "bracketed root label and sequence item",
            r#"tasks: {
    "sequence[0]": schema.#Task & {command: "echo"}
    sequence: schema.#TaskSequence & [
        schema.#Task & {command: "echo", dependsOn: [tasks["sequence[0]"].stdout]},
    ]
}"#,
            "tasks.sequence[0].dependsOn[0]",
        ),
    ];

    for (label, tasks, expected_path) in cases {
        let source = format!(
            r#"package fixture

import "github.com/cuenv/cuenv/schema"

schema.#Project
name: "path-identity-test"
{tasks}
"#
        );
        assert_evaluation_fails_with(&source, label, &[expected_path])?;
    }

    Ok(())
}

#[test]
fn cyclic_task_references_fail_at_the_unvalidated_back_edge() -> TestResult {
    let source = r#"package fixture

import "github.com/cuenv/cuenv/schema"

schema.#Project
name: "cyclic-task-reference-test"
tasks: {
    first: schema.#Task & {command: "echo", dependsOn: [second]}
    second: schema.#Task & {command: "echo", dependsOn: [first]}
}
"#;

    let message = evaluation_failure_message(source, "cyclic task references")?;
    for fragment in [
        "tasks.first.dependsOn[0].dependsOn[0]",
        "dependency is not a validated CUE task",
    ] {
        if !message.contains(fragment) {
            return Err(std::io::Error::other(format!(
                "cyclic task references did not fail at the unvalidated back edge; missing {fragment:?}: {message}"
            ))
            .into());
        }
    }

    Ok(())
}

#[test]
fn dependency_sequences_validate_nested_task_edges() -> TestResult {
    let cases = [
        (
            "task dependency inside a sequence",
            r#"[{command: "echo", dependsOn: [tasks.build.stdout]}]"#,
            "tasks.consumer.dependsOn[0][0].dependsOn[0]",
        ),
        (
            "group child dependency inside a sequence",
            r#"[{type: "group", child: {command: "echo", dependsOn: [tasks.build.stdout]}}]"#,
            "tasks.consumer.dependsOn[0][0].child.dependsOn[0]",
        ),
    ];

    for (label, nodes, expected_path) in cases {
        let source = format!(
            r#"package fixture

import "github.com/cuenv/cuenv/schema"

#BadSequence: schema.#TaskSequence & {nodes}

schema.#Project
name: "nested-dependency-sequence-test"
tasks: {{
    build: schema.#Task & {{command: "echo"}}
    consumer: schema.#Task & {{command: "echo", dependsOn: [#BadSequence]}}
}}
"#
        );

        assert_evaluation_fails_with(&source, label, &[expected_path])?;
    }

    Ok(())
}

#[test]
fn individual_sequence_dependency_elements_validate_nested_task_edges() -> TestResult {
    let cases = [
        (
            "task element dependency",
            r#"#BadSequence: schema.#TaskSequence & [{command: "echo", dependsOn: [tasks.build.stdout]}]"#,
            "#BadSequence[0]",
            "tasks.consumer.dependsOn[0].dependsOn[0]",
        ),
        (
            "group element dependency",
            r#"#BadSequence: schema.#TaskSequence & [{type: "group", child: {command: "echo", dependsOn: [tasks.build.stdout]}}]"#,
            "#BadSequence[0]",
            "tasks.consumer.dependsOn[0].child.dependsOn[0]",
        ),
        (
            "sequence group child dependency",
            r#"#BadSequence: schema.#TaskSequence & [{type: "group", child: {command: "echo", dependsOn: [tasks.build.stdout]}}]"#,
            "#BadSequence[0].child",
            "tasks.consumer.dependsOn[0].dependsOn[0]",
        ),
        (
            "standalone group child dependency",
            r#"#BadGroup: schema.#TaskGroup & {type: "group", child: {command: "echo", dependsOn: [tasks.build.stdout]}}"#,
            "#BadGroup.child",
            "tasks.consumer.dependsOn[0].dependsOn[0]",
        ),
    ];

    for (label, definition, dependency, expected_path) in cases {
        let source = format!(
            r#"package fixture

import "github.com/cuenv/cuenv/schema"

schema.#Project
name: "sequence-element-reference-test"
tasks: {{
    build: schema.#Task & {{command: "echo"}}
    consumer: schema.#Task & {{command: "echo", dependsOn: [{dependency}]}}
}}

{definition}
"#
        );

        assert_evaluation_fails_with(&source, label, &[expected_path])?;
    }

    Ok(())
}

#[test]
fn nested_task_fields_validate_weakened_dependency_edges() -> TestResult {
    let cases = [
        (
            "underscore-prefixed task group child",
            r#"package fixture

import "github.com/cuenv/cuenv/schema"

schema.#Project
name: "underscore-child-negative-test"
tasks: {
    build: schema.#Task & {command: "echo"}
    group: schema.#TaskGroup & {
        type: "group"
        "_step": schema.#Task & {command: "echo", dependsOn: [tasks.build.stdout]}
    }
}
"#,
            "tasks.group._step.dependsOn",
        ),
        (
            "service task entrypoint",
            r#"package fixture

import "github.com/cuenv/cuenv/schema"

schema.#Project
name: "service-entrypoint-negative-test"
tasks: {
    build: schema.#Task & {command: "echo"}
}
services: {
    api: schema.#Service & {
        entrypoint: schema.#Task & {command: "echo", dependsOn: [tasks.build.stdout]}
    }
}
"#,
            "services.api.entrypoint.dependsOn",
        ),
        (
            "service watcher rebuild task",
            r#"package fixture

import "github.com/cuenv/cuenv/schema"

schema.#Project
name: "service-rebuild-negative-test"
tasks: {
    build: schema.#Task & {command: "echo"}
}
services: {
    api: schema.#Service & {
        entrypoint: schema.#Command & {command: "echo"}
        watch: {
            paths: ["src/**"]
            rebuild: [schema.#Task & {command: "echo", dependsOn: [tasks.build.stdout]}]
        }
    }
}
"#,
            "services.api.watch.rebuild[0].dependsOn",
        ),
    ];

    for (label, source, expected_path) in cases {
        assert_evaluation_fails_with(source, label, &[expected_path])?;
    }

    Ok(())
}

#[test]
fn service_entrypoint_and_watcher_rebuild_accept_validated_tasks() -> TestResult {
    let result = evaluate_source(
        r#"package fixture

import "github.com/cuenv/cuenv/schema"

schema.#Project
name: "service-task-fields-valid"
tasks: {
    build: schema.#Task & {command: "echo"}
}
services: {
    api: schema.#Service & {
        entrypoint: schema.#Task & {command: "echo", dependsOn: [tasks.build]}
        watch: {
            paths: ["src/**"]
            rebuild: [
                schema.#Task & {command: "echo", dependsOn: [tasks.build]},
                schema.#TaskGroup & {
                    type: "group"
                    "_step": schema.#Task & {command: "echo", dependsOn: [tasks.build]}
                },
            ]
        }
    }
}
"#,
    )?;
    assert_eq!(result.projects.len(), 1);
    Ok(())
}

fn strip_task_reference_prefix(reference: &str) -> &str {
    for prefix in ["tasks.", "_tasks.", "_t."] {
        if let Some(stripped) = reference.strip_prefix(prefix) {
            return stripped;
        }
    }
    reference
}
