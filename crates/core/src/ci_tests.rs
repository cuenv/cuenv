use super::*;

#[test]
fn test_string_or_vec() {
    let single = StringOrVec::String("value".to_string());
    assert_eq!(single.to_vec(), vec!["value"]);
    assert_eq!(single.as_single(), Some("value"));

    let multi = StringOrVec::Vec(vec!["a".to_string(), "b".to_string()]);
    assert_eq!(multi.to_vec(), vec!["a", "b"]);
    assert_eq!(multi.as_single(), Some("a"));
}

#[test]
fn test_manual_trigger_bool() {
    let json = r#"{"manual": true}"#;
    let cond: PipelineCondition = serde_json::from_str(json).unwrap();
    assert!(matches!(cond.manual, Some(ManualTrigger::Enabled(true))));

    let json = r#"{"manual": false}"#;
    let cond: PipelineCondition = serde_json::from_str(json).unwrap();
    assert!(matches!(cond.manual, Some(ManualTrigger::Enabled(false))));
}

#[test]
fn test_manual_trigger_with_inputs() {
    let json = r#"{"manual": {"tag_name": {"description": "Tag to release", "required": true}}}"#;
    let cond: PipelineCondition = serde_json::from_str(json).unwrap();

    match &cond.manual {
        Some(ManualTrigger::WithInputs(inputs)) => {
            assert!(inputs.contains_key("tag_name"));
            let input = inputs.get("tag_name").unwrap();
            assert_eq!(input.description, "Tag to release");
            assert_eq!(input.required, Some(true));
        }
        _ => panic!("Expected WithInputs variant"),
    }
}

#[test]
fn test_manual_trigger_helpers() {
    let enabled = ManualTrigger::Enabled(true);
    assert!(enabled.is_enabled());
    assert!(enabled.inputs().is_none());

    let disabled = ManualTrigger::Enabled(false);
    assert!(!disabled.is_enabled());

    let mut inputs = HashMap::new();
    inputs.insert(
        "tag".to_string(),
        WorkflowDispatchInput {
            description: "Tag name".to_string(),
            required: Some(true),
            default: None,
            input_type: None,
            options: None,
        },
    );
    let with_inputs = ManualTrigger::WithInputs(inputs);
    assert!(with_inputs.is_enabled());
    assert!(with_inputs.inputs().is_some());
}

#[test]
fn test_scheduled_cron_expressions() {
    // Single cron expression
    let json = r#"{"scheduled": "0 0 * * 0"}"#;
    let cond: PipelineCondition = serde_json::from_str(json).unwrap();
    match &cond.scheduled {
        Some(StringOrVec::String(s)) => assert_eq!(s, "0 0 * * 0"),
        _ => panic!("Expected single string"),
    }

    // Multiple cron expressions
    let json = r#"{"scheduled": ["0 0 * * 0", "0 12 * * *"]}"#;
    let cond: PipelineCondition = serde_json::from_str(json).unwrap();
    match &cond.scheduled {
        Some(StringOrVec::Vec(v)) => {
            assert_eq!(v.len(), 2);
            assert_eq!(v[0], "0 0 * * 0");
            assert_eq!(v[1], "0 12 * * *");
        }
        _ => panic!("Expected vec"),
    }
}

#[test]
fn test_release_trigger() {
    let json = r#"{"release": ["published", "created"]}"#;
    let cond: PipelineCondition = serde_json::from_str(json).unwrap();
    assert_eq!(
        cond.release,
        Some(vec!["published".to_string(), "created".to_string()])
    );
}

#[test]
fn test_pipeline_derive_paths() {
    // Tasks are CUE refs (objects with _name) after enrichment
    let json = r#"{"tasks": [{"_name": "test"}], "derivePaths": true}"#;
    let pipeline: Pipeline = serde_json::from_str(json).unwrap();
    assert_eq!(pipeline.derive_paths, Some(true));

    let json = r#"{"tasks": [{"_name": "sync"}], "derivePaths": false}"#;
    let pipeline: Pipeline = serde_json::from_str(json).unwrap();
    assert_eq!(pipeline.derive_paths, Some(false));

    let json = r#"{"tasks": [{"_name": "build"}]}"#;
    let pipeline: Pipeline = serde_json::from_str(json).unwrap();
    assert_eq!(pipeline.derive_paths, None);
}

#[test]
fn test_pipeline_task_simple() {
    // CUE ref enriched with _name
    let json = r#"{"_name": "build", "command": "cargo build"}"#;
    let task: PipelineTask = serde_json::from_str(json).unwrap();
    assert!(matches!(task, PipelineTask::Simple(_)));
    assert_eq!(task.task_name(), "build");
    assert!(!task.is_matrix());
    assert!(task.matrix().is_none());
}

#[test]
fn test_pipeline_task_matrix() {
    // Matrix task with CUE ref (object with _name) and type discriminator
    let json = r#"{"type": "matrix", "task": {"_name": "release.build"}, "matrix": {"arch": ["linux-x64", "darwin-arm64"]}}"#;
    let task: PipelineTask = serde_json::from_str(json).unwrap();
    assert!(task.is_matrix());
    assert_eq!(task.task_name(), "release.build");

    let matrix = task.matrix().unwrap();
    assert!(matrix.contains_key("arch"));
    assert_eq!(matrix["arch"], vec!["linux-x64", "darwin-arm64"]);
}

#[test]
fn test_pipeline_task_matrix_with_artifacts() {
    let json = r#"{
            "type": "matrix",
            "task": {"_name": "release.publish"},
            "matrix": {},
            "artifacts": [{"from": "release.build", "to": "dist", "filter": "*stable"}],
            "params": {"tag": "v1.0.0"}
        }"#;
    let task: PipelineTask = serde_json::from_str(json).unwrap();

    if let PipelineTask::Matrix(m) = task {
        assert_eq!(m.task.task_name(), "release.publish");
        let artifacts = m.artifacts.unwrap();
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].from, "release.build");
        assert_eq!(artifacts[0].to, "dist");
        assert_eq!(artifacts[0].filter, "*stable");

        let params = m.params.unwrap();
        assert_eq!(params.get("tag"), Some(&"v1.0.0".to_string()));
    } else {
        panic!("Expected Matrix variant");
    }
}

#[test]
fn test_pipeline_mixed_tasks() {
    // Mix of matrix and simple tasks (CUE ref format only)
    let json = r#"{
            "tasks": [
                {"type": "matrix", "task": {"_name": "release.build"}, "matrix": {"arch": ["linux-x64", "darwin-arm64"]}},
                {"_name": "release.publish:github"},
                {"_name": "docs.deploy"}
            ]
        }"#;
    let pipeline: Pipeline = serde_json::from_str(json).unwrap();
    assert_eq!(pipeline.tasks.len(), 3);
    assert!(pipeline.tasks[0].is_matrix());
    assert!(!pipeline.tasks[1].is_matrix());
    assert!(!pipeline.tasks[2].is_matrix());
}

#[test]
fn test_runner_mapping() {
    let json = r#"{"arch": {"linux-x64": "ubuntu-latest", "darwin-arm64": "macos-14"}}"#;
    let mapping: RunnerMapping = serde_json::from_str(json).unwrap();
    let arch = mapping.arch.unwrap();
    assert_eq!(arch.get("linux-x64"), Some(&"ubuntu-latest".to_string()));
    assert_eq!(arch.get("darwin-arm64"), Some(&"macos-14".to_string()));
}

#[test]
fn test_contributor_task_with_command_and_args() {
    let json = r#"{
            "id": "bun.workspace.install",
            "command": "bun",
            "args": ["install", "--frozen-lockfile"],
            "inputs": ["package.json", "bun.lock"],
            "outputs": ["node_modules"]
        }"#;
    let task: ContributorTask = serde_json::from_str(json).unwrap();
    assert_eq!(task.id, "bun.workspace.install");
    assert_eq!(task.command, Some("bun".to_string()));
    assert_eq!(task.args, vec!["install", "--frozen-lockfile"]);
    assert_eq!(task.inputs, vec!["package.json", "bun.lock"]);
    assert_eq!(task.outputs, vec!["node_modules"]);
}

#[test]
fn test_contributor_task_with_script() {
    let json = r#"{
            "id": "nix.install",
            "command": "sh",
            "args": ["-c", "curl -sSL https://install.determinate.systems/nix | sh"]
        }"#;
    let task: ContributorTask = serde_json::from_str(json).unwrap();
    assert_eq!(task.id, "nix.install");
    assert_eq!(task.command, Some("sh".to_string()));
    assert_eq!(
        task.args,
        vec![
            "-c",
            "curl -sSL https://install.determinate.systems/nix | sh"
        ]
    );
}

#[test]
fn test_contributor_with_auto_associate() {
    let json = r#"{
            "id": "bun.workspace",
            "when": {"workspaceMember": ["bun"]},
            "tasks": [{
                "id": "bun.workspace.install",
                "command": "bun",
                "args": ["install"]
            }],
            "autoAssociate": {
                "command": ["bun", "bunx"],
                "injectDependency": "cuenv:contributor:bun.workspace.setup"
            }
        }"#;
    let contributor: Contributor = serde_json::from_str(json).unwrap();
    assert_eq!(contributor.id, "bun.workspace");

    let when = contributor.when.unwrap();
    assert_eq!(when.workspace_member, vec!["bun"]);

    let auto = contributor.auto_associate.unwrap();
    assert_eq!(auto.command, vec!["bun", "bunx"]);
    assert_eq!(
        auto.inject_dependency,
        Some("cuenv:contributor:bun.workspace.setup".to_string())
    );
}

#[test]
fn test_activation_condition_workspace_member() {
    let json = r#"{"workspaceMember": ["npm", "bun"]}"#;
    let cond: ActivationCondition = serde_json::from_str(json).unwrap();
    assert_eq!(cond.workspace_member, vec!["npm", "bun"]);
}

#[test]
fn test_providers_for_pipeline_global() {
    let ci = CI {
        providers: vec!["github".to_string()],
        pipelines: BTreeMap::from([(
            "ci".to_string(),
            Pipeline {
                providers: vec![],
                mode: PipelineMode::default(),
                environment: None,
                when: None,
                tasks: vec![],
                annotations: HashMap::new(),
                continue_on_error: false,
                derive_paths: None,
                provider: None,
            },
        )]),
        ..Default::default()
    };
    assert_eq!(ci.providers_for_pipeline("ci"), &["github"]);
}

#[test]
fn test_providers_for_pipeline_override() {
    let ci = CI {
        providers: vec!["github".to_string()],
        pipelines: BTreeMap::from([(
            "release".to_string(),
            Pipeline {
                providers: vec!["buildkite".to_string()],
                mode: PipelineMode::default(),
                environment: None,
                when: None,
                tasks: vec![],
                annotations: HashMap::new(),
                continue_on_error: false,
                derive_paths: None,
                provider: None,
            },
        )]),
        ..Default::default()
    };
    assert_eq!(ci.providers_for_pipeline("release"), &["buildkite"]);
}

#[test]
fn test_providers_for_pipeline_empty() {
    let ci = CI::default();
    assert!(ci.providers_for_pipeline("any").is_empty());
}

#[test]
fn test_providers_for_pipeline_nonexistent() {
    let ci = CI {
        providers: vec!["github".to_string()],
        ..Default::default()
    };
    // Non-existent pipeline falls back to global
    assert_eq!(ci.providers_for_pipeline("nonexistent"), &["github"]);
}

#[test]
fn test_pipeline_task_node_task_group() {
    // Inline TaskGroup definition (has type: "group" and child tasks)
    let json = r#"{
            "type": "group",
            "http": {
                "command": "bun",
                "args": ["x", "wrangler", "deploy"]
            }
        }"#;
    let task: PipelineTask = serde_json::from_str(json).unwrap();
    assert!(task.is_node());
    assert!(!task.is_matrix());
    assert!(!task.is_simple());
    // For groups, task_name returns the first child's name
    assert_eq!(task.task_name(), "http");
    // Child task names should include "http"
    let children = task.child_task_names();
    assert!(children.contains(&"http"));
}

#[test]
fn test_pipeline_task_node_inline_task() {
    // Inline Task definition (no _name, has command)
    let json = r#"{
            "command": "echo",
            "args": ["hello"],
            "description": "Say hello"
        }"#;
    let task: PipelineTask = serde_json::from_str(json).unwrap();
    assert!(task.is_node());
    // For inline tasks without _name, task_name falls back to description
    assert_eq!(task.task_name(), "Say hello");
}

#[test]
fn test_pipeline_mixed_with_node() {
    // Mix of Simple, Matrix, and Node tasks
    let json = r#"{
            "tasks": [
                {"_name": "build"},
                {"type": "matrix", "task": {"_name": "release"}, "matrix": {}},
                {"type": "group", "deploy": {"command": "deploy"}}
            ]
        }"#;
    let pipeline: Pipeline = serde_json::from_str(json).unwrap();
    assert_eq!(pipeline.tasks.len(), 3);
    assert!(pipeline.tasks[0].is_simple());
    assert!(pipeline.tasks[1].is_matrix());
    assert!(pipeline.tasks[2].is_node());
}

#[test]
fn test_annotation_value_serde_roundtrip() {
    // Literal
    let literal = AnnotationValue::Literal("hello".to_string());
    let json = serde_json::to_string(&literal).unwrap();
    let deserialized: AnnotationValue = serde_json::from_str(&json).unwrap();
    assert_eq!(literal, deserialized);

    // CaptureRef
    let capture_ref = AnnotationValue::CaptureRef {
        cuenv_capture_ref: true,
        cuenv_task: "deploy.preview".to_string(),
        cuenv_capture: "previewUrl".to_string(),
    };
    let json = serde_json::to_string(&capture_ref).unwrap();
    assert!(json.contains("cuenvCaptureRef"));
    assert!(json.contains("cuenvTask"));
    assert!(json.contains("cuenvCapture"));
    let deserialized: AnnotationValue = serde_json::from_str(&json).unwrap();
    assert_eq!(capture_ref, deserialized);
}

#[test]
fn test_pipeline_with_annotations() {
    let json = r#"{
            "tasks": [{"_name": "deploy"}],
            "annotations": {
                "Preview URL": {"cuenvCaptureRef": true, "cuenvTask": "deploy.preview", "cuenvCapture": "previewUrl"},
                "Version": "1.0.0"
            }
        }"#;
    let pipeline: Pipeline = serde_json::from_str(json).unwrap();
    assert_eq!(pipeline.annotations.len(), 2);
    assert!(matches!(
        pipeline.annotations.get("Version"),
        Some(AnnotationValue::Literal(s)) if s == "1.0.0"
    ));
    assert!(matches!(
        pipeline.annotations.get("Preview URL"),
        Some(AnnotationValue::CaptureRef { cuenv_task, cuenv_capture, .. })
        if cuenv_task == "deploy.preview" && cuenv_capture == "previewUrl"
    ));
}
