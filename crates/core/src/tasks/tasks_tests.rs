use super::{
    FilesAnchor, FilesReference, Input, InputPattern, ParamDef, ParamType, ResolvedArgs,
    ScriptShell, ShellOptionToggle, ShellOptions, Task, TaskCacheMode, TaskGroup, TaskNode,
    TaskParams, Tasks,
};
use std::collections::HashMap;
use std::path::Path;

#[test]
fn test_task_default_values() {
    let task = Task {
        command: "echo".to_string(),
        ..Default::default()
    };

    assert!(task.shell.is_none());
    assert_eq!(task.command, "echo");
    assert_eq!(task.description(), "No description provided");
    assert!(task.args.is_empty());
    assert!(task.is_hermetic()); // default is true
}

#[test]
fn test_task_deserialization() {
    let json = r#"{
        "command": "echo",
        "args": ["Hello", "World"]
    }"#;

    let task: Task = serde_json::from_str(json).unwrap();
    assert_eq!(task.command, "echo");
    assert_eq!(task.args, vec!["Hello", "World"]);
    assert!(task.shell.is_none()); // default value
}

#[test]
fn test_task_script_deserialization() {
    // Test that script-only tasks (no command) deserialize correctly
    let json = r#"{
        "script": "echo hello",
        "inputs": ["src/main.rs"]
    }"#;

    let task: Task = serde_json::from_str(json).unwrap();
    assert!(task.command.is_empty()); // No command
    assert_eq!(task.script, Some("echo hello".to_string()));
    assert_eq!(task.inputs.len(), 1);
}

#[test]
fn test_task_cache_policy_defaults_to_never() {
    let task = Task {
        command: "echo".to_string(),
        ..Default::default()
    };

    let policy = task.cache_policy();
    assert_eq!(policy.mode, TaskCacheMode::Never);
    assert!(policy.max_age.is_none());
}

#[test]
fn test_task_cache_policy_deserialization() {
    let json = r#"{
        "command": "echo",
        "cache": {
            "mode": "read-write",
            "maxAge": "1h"
        }
    }"#;

    let task: Task = serde_json::from_str(json).unwrap();
    let policy = task.cache_policy();
    assert_eq!(policy.mode, TaskCacheMode::ReadWrite);
    assert_eq!(policy.max_age, Some("1h".to_string()));
}

#[test]
fn test_task_node_script_variant() {
    // Test that TaskNode::Task correctly deserializes script-only tasks
    let json = r#"{
        "script": "echo hello"
    }"#;

    let node: TaskNode = serde_json::from_str(json).unwrap();
    assert!(node.is_task());
}

#[test]
fn test_task_group_with_script_task() {
    // Test parallel task group containing a script task (mimics cross.linux)
    // TaskGroup uses type: "group" discriminator with flattened children
    let json = r#"{
        "type": "group",
        "linux": {
            "script": "echo building",
            "inputs": ["src/main.rs"]
        }
    }"#;

    let group: TaskGroup = serde_json::from_str(json).unwrap();
    assert_eq!(group.len(), 1);
}

#[test]
fn test_full_tasks_map_with_script() {
    // Test deserializing a full tasks map like in Project.tasks
    // TaskGroup uses type: "group" with flattened children
    let json = r#"{
        "pwd": { "command": "pwd" },
        "cross": {
            "type": "group",
            "linux": {
                "script": "echo building",
                "inputs": ["src/main.rs"]
            }
        }
    }"#;

    let tasks: HashMap<String, TaskNode> = serde_json::from_str(json).unwrap();
    assert_eq!(tasks.len(), 2);
    assert!(tasks.contains_key("pwd"));
    assert!(tasks.contains_key("cross"));

    // pwd should be Task
    assert!(tasks.get("pwd").unwrap().is_task());

    // cross should be Group
    assert!(tasks.get("cross").unwrap().is_group());
}

#[test]
fn test_complex_nested_tasks_like_cuenv() {
    // Test a more complex structure mimicking cuenv's actual env.cue tasks
    // TaskGroup uses type: "group" with flattened children
    let json = r#"{
        "pwd": { "command": "pwd" },
        "check": {
            "command": "nix",
            "args": ["flake", "check"],
            "inputs": ["flake.nix"]
        },
        "fmt": {
            "type": "group",
            "fix": {
                "command": "treefmt",
                "inputs": [".config"]
            },
            "check": {
                "command": "treefmt",
                "args": ["--fail-on-change"],
                "inputs": [".config"]
            }
        },
        "cross": {
            "type": "group",
            "linux": {
                "script": "echo building",
                "inputs": ["Cargo.toml"]
            }
        },
        "docs": {
            "type": "group",
            "build": {
                "command": "bash",
                "args": ["-c", "bun install"],
                "inputs": ["docs"],
                "outputs": ["docs/dist"]
            },
            "deploy": {
                "command": "bash",
                "args": ["-c", "wrangler deploy"],
                "dependsOn": ["docs.build"],
                "inputs": [{"task": "docs.build"}]
            }
        }
    }"#;

    let result: Result<HashMap<String, TaskNode>, _> = serde_json::from_str(json);
    match result {
        Ok(tasks) => {
            assert_eq!(tasks.len(), 5);
            assert!(tasks.get("pwd").unwrap().is_task());
            assert!(tasks.get("check").unwrap().is_task());
            assert!(tasks.get("fmt").unwrap().is_group());
            assert!(tasks.get("cross").unwrap().is_group());
            assert!(tasks.get("docs").unwrap().is_group());
        }
        Err(e) => {
            panic!("Failed to deserialize complex tasks: {}", e);
        }
    }
}

#[test]
fn test_task_list_sequential() {
    let task1 = Task {
        command: "echo".to_string(),
        args: vec!["first".to_string()],
        description: Some("First task".to_string()),
        ..Default::default()
    };

    let task2 = Task {
        command: "echo".to_string(),
        args: vec!["second".to_string()],
        description: Some("Second task".to_string()),
        ..Default::default()
    };

    let sequence: Vec<TaskNode> = vec![
        TaskNode::Task(Box::new(task1)),
        TaskNode::Task(Box::new(task2)),
    ];

    assert_eq!(sequence.len(), 2);
    assert!(!sequence.is_empty());
}

#[test]
fn test_task_group_parallel() {
    let task1 = Task {
        command: "echo".to_string(),
        args: vec!["task1".to_string()],
        description: Some("Task 1".to_string()),
        ..Default::default()
    };

    let task2 = Task {
        command: "echo".to_string(),
        args: vec!["task2".to_string()],
        description: Some("Task 2".to_string()),
        ..Default::default()
    };

    let mut parallel_tasks = HashMap::new();
    parallel_tasks.insert("task1".to_string(), TaskNode::Task(Box::new(task1)));
    parallel_tasks.insert("task2".to_string(), TaskNode::Task(Box::new(task2)));

    let group = TaskGroup {
        type_: "group".to_string(),
        children: parallel_tasks,
        depends_on: vec![],
        max_concurrency: None,
        description: None,
    };

    assert_eq!(group.len(), 2);
    assert!(!group.is_empty());
}

#[test]
fn test_tasks_collection() {
    let mut tasks = Tasks::new();
    assert!(tasks.list_tasks().is_empty());

    let task = Task {
        command: "echo".to_string(),
        args: vec!["hello".to_string()],
        description: Some("Hello task".to_string()),
        ..Default::default()
    };

    tasks
        .tasks
        .insert("greet".to_string(), TaskNode::Task(Box::new(task)));

    assert!(tasks.contains("greet"));
    assert!(!tasks.contains("nonexistent"));
    assert_eq!(tasks.list_tasks(), vec!["greet"]);

    let retrieved = tasks.get("greet").unwrap();
    assert!(retrieved.is_task());
}

#[test]
fn test_task_node_helpers() {
    let task = Task {
        command: "test".to_string(),
        description: Some("Test task".to_string()),
        ..Default::default()
    };

    let task_node = TaskNode::Task(Box::new(task.clone()));
    assert!(task_node.is_task());
    assert!(!task_node.is_group());
    assert!(!task_node.is_sequence());
    assert_eq!(task_node.as_task().unwrap().command, "test");
    assert!(task_node.as_group().is_none());
    assert!(task_node.as_sequence().is_none());

    let group = TaskNode::Group(TaskGroup {
        type_: "group".to_string(),
        children: HashMap::new(),
        depends_on: vec![],
        max_concurrency: None,
        description: None,
    });
    assert!(!group.is_task());
    assert!(group.is_group());
    assert!(!group.is_sequence());
    assert!(group.as_task().is_none());
    assert!(group.as_group().is_some());

    let sequence = TaskNode::Sequence(vec![]);
    assert!(!sequence.is_task());
    assert!(!sequence.is_group());
    assert!(sequence.is_sequence());
    assert!(sequence.as_sequence().is_some());
}

#[test]
fn test_script_shell_command_and_flag() {
    assert_eq!(ScriptShell::Bash.command_and_flag(), ("bash", "-c"));
    assert_eq!(ScriptShell::Nu.command_and_flag(), ("nu", "-c"));
    assert_eq!(ScriptShell::Python.command_and_flag(), ("python", "-c"));
    assert_eq!(ScriptShell::Node.command_and_flag(), ("node", "-e"));
    assert_eq!(
        ScriptShell::Powershell.command_and_flag(),
        ("powershell", "-Command")
    );
}

#[test]
fn test_script_shell_from_command() {
    assert_eq!(
        ScriptShell::from_command("/usr/bin/nu"),
        Some(ScriptShell::Nu)
    );
    assert_eq!(
        ScriptShell::from_command("pwsh.exe"),
        Some(ScriptShell::Pwsh)
    );
    assert_eq!(ScriptShell::from_command("custom-shell"), None);
}

#[test]
fn test_shell_options_default() {
    let opts = ShellOptions::default();
    assert!(opts.errexit.is_enabled());
    assert!(opts.nounset.is_enabled());
    assert!(opts.pipefail.is_enabled());
    assert!(!opts.xtrace.is_enabled());
}

#[test]
fn test_shell_options_to_set_commands() {
    let opts = ShellOptions::default();
    assert_eq!(opts.to_set_commands(), "set -e -u -o pipefail\n");

    let debug_opts = ShellOptions {
        errexit: ShellOptionToggle::Enabled,
        nounset: ShellOptionToggle::Disabled,
        pipefail: ShellOptionToggle::Enabled,
        xtrace: ShellOptionToggle::Enabled,
    };
    assert_eq!(debug_opts.to_set_commands(), "set -e -o pipefail -x\n");

    let no_opts = ShellOptions {
        errexit: ShellOptionToggle::Disabled,
        nounset: ShellOptionToggle::Disabled,
        pipefail: ShellOptionToggle::Disabled,
        xtrace: ShellOptionToggle::Disabled,
    };
    assert_eq!(no_opts.to_set_commands(), "");
}

#[test]
fn test_input_deserialization_variants() {
    let path_json = r#""src/**/*.rs""#;
    let path_input: Input = serde_json::from_str(path_json).unwrap();
    assert_eq!(path_input, Input::Path("src/**/*.rs".to_string()));

    // Test TaskOutput variant (same-project task reference)
    let task_json = r#"{"task": "build.deps"}"#;
    let task_input: Input = serde_json::from_str(task_json).unwrap();
    match task_input {
        Input::Task(output) => {
            assert_eq!(output.task, "build.deps");
            assert!(output.map.is_none());
        }
        other => panic!("Expected task output reference, got {:?}", other),
    }

    // A task output of another project, as `outputFiles` is read.
    let external: Input = serde_json::from_str(r##"{"task": "#content:build"}"##).unwrap();
    assert_eq!(external.task_output_name(), Some("#content:build"));
}

#[test]
fn the_removed_project_reference_is_never_read_as_a_task_output() {
    // `{project, task, map}` was removed. Read as a task output it would
    // name a same-named task of this project.
    let removed = serde_json::from_str::<Input>(
        r#"{"project": "../projB", "task": "build", "map": [{"from": "a", "to": "b"}]}"#,
    );
    assert!(removed.is_err(), "{removed:?}");
}

#[test]
fn a_task_reading_only_task_outputs_is_affected_through_its_producers() {
    use crate::AffectedBy;

    let task = Task {
        inputs: vec![Input::Task(super::TaskOutput {
            task: "#content:build".into(),
            map: None,
        })],
        ..Default::default()
    };
    let anchors = crate::NoAnchors;
    let frame = crate::InputFrame {
        project_root: Path::new("app"),
        anchors: &anchors,
    };

    assert!(!task.is_affected_by(&[std::path::PathBuf::from("app/README.md")], frame));
    let no_inputs = Task::default();
    assert!(no_inputs.is_affected_by(&[std::path::PathBuf::from("app/README.md")], frame));
}

#[test]
fn test_resolved_args_interpolate_positional() {
    let args = ResolvedArgs {
        positional: vec!["video123".into(), "1080p".into()],
        named: HashMap::new(),
    };
    assert_eq!(args.interpolate("{{0}}"), "video123");
    assert_eq!(args.interpolate("{{1}}"), "1080p");
    assert_eq!(args.interpolate("--id={{0}}"), "--id=video123");
    assert_eq!(args.interpolate("{{0}}-{{1}}"), "video123-1080p");
}

#[test]
fn test_resolved_args_interpolate_named() {
    let mut named = HashMap::new();
    named.insert("url".into(), "https://example.com".into());
    named.insert("quality".into(), "720p".into());
    let args = ResolvedArgs {
        positional: vec![],
        named,
    };
    assert_eq!(args.interpolate("{{url}}"), "https://example.com");
    assert_eq!(args.interpolate("--quality={{quality}}"), "--quality=720p");
}

#[test]
fn test_resolved_args_interpolate_mixed() {
    let mut named = HashMap::new();
    named.insert("format".into(), "mp4".into());
    let args = ResolvedArgs {
        positional: vec!["VIDEO_ID".into()],
        named,
    };
    assert_eq!(
        args.interpolate("download {{0}} --format={{format}}"),
        "download VIDEO_ID --format=mp4"
    );
}

#[test]
fn test_resolved_args_no_placeholder_unchanged() {
    let args = ResolvedArgs::new();
    assert_eq!(
        args.interpolate("no placeholders here"),
        "no placeholders here"
    );
    assert_eq!(args.interpolate(""), "");
}

#[test]
fn test_resolved_args_interpolate_args_list() {
    let args = ResolvedArgs {
        positional: vec!["id123".into()],
        named: HashMap::new(),
    };
    let input = vec!["--id".into(), "{{0}}".into(), "--verbose".into()];
    let result = args.interpolate_args(&input);
    assert_eq!(result, vec!["--id", "id123", "--verbose"]);
}

#[test]
fn test_task_params_deserialization_with_flatten() {
    // Test that named params are flattened (not nested under "named")
    let json = r#"{
        "positional": [{"description": "Video ID", "required": true}],
        "quality": {"description": "Quality", "default": "1080p", "short": "q"},
        "verbose": {"description": "Verbose output", "type": "bool"}
    }"#;
    let params: TaskParams = serde_json::from_str(json).unwrap();

    assert_eq!(params.positional.len(), 1);
    assert_eq!(
        params.positional[0].description,
        Some("Video ID".to_string())
    );
    assert!(params.positional[0].required);

    assert_eq!(params.named.len(), 2);
    assert!(params.named.contains_key("quality"));
    assert!(params.named.contains_key("verbose"));

    let quality = &params.named["quality"];
    assert_eq!(quality.default, Some("1080p".to_string()));
    assert_eq!(quality.short, Some("q".to_string()));

    let verbose = &params.named["verbose"];
    assert_eq!(verbose.param_type, ParamType::Bool);
}

#[test]
fn test_task_params_empty() {
    let json = r#"{}"#;
    let params: TaskParams = serde_json::from_str(json).unwrap();
    assert!(params.positional.is_empty());
    assert!(params.named.is_empty());
}

#[test]
fn test_param_def_defaults() {
    let def = ParamDef::default();
    assert!(def.description.is_none());
    assert!(!def.required);
    assert!(def.default.is_none());
    assert_eq!(def.param_type, ParamType::String);
    assert!(def.short.is_none());
}

// ==========================================================================
// AffectedBy trait tests
// ==========================================================================

mod affected_tests {
    use super::*;
    use crate::{AffectedBy, InputFrame, NoAnchors, PlaceAnchor};
    use std::path::PathBuf;

    /// The inputs of a project at `root`, with no file reference placed.
    fn frame(root: &Path) -> InputFrame<'_> {
        InputFrame {
            project_root: root,
            anchors: &NoAnchors,
        }
    }

    /// `content` lives in `content/`.
    struct Content;

    impl PlaceAnchor for Content {
        fn place(&self, anchor: &FilesAnchor) -> Option<PathBuf> {
            match anchor {
                FilesAnchor::Project(name) if name == "content" => Some(PathBuf::from("content")),
                FilesAnchor::Project(_) | FilesAnchor::Module => None,
            }
        }
    }

    fn reading_articles() -> Task {
        Task {
            inputs: vec![Input::Files(FilesReference {
                anchor: FilesAnchor::Project("content".to_string()),
                paths: vec!["articles/**".to_string()],
            })],
            command: "echo test".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn a_task_whose_only_inputs_are_file_references_is_matched_by_them() {
        let task = reading_articles();
        let frame = InputFrame {
            project_root: Path::new("app"),
            anchors: &Content,
        };

        assert!(task.is_affected_by(&[PathBuf::from("content/articles/one.md")], frame));
        assert!(
            !task.is_affected_by(&[PathBuf::from("app/README.md")], frame),
            "a file reference is an input: the task is not affected by every change"
        );
        assert_eq!(
            AffectedBy::input_patterns(&task),
            [InputPattern::Anchored {
                anchor: &FilesAnchor::Project("content".to_string()),
                path: "articles/**"
            }]
        );
    }

    #[test]
    fn a_file_reference_that_cannot_be_placed_fails_closed() {
        let task = reading_articles();

        assert!(task.is_affected_by(&[PathBuf::from("app/README.md")], frame(Path::new("app"))));
    }

    fn make_task(inputs: Vec<&str>) -> Task {
        Task {
            inputs: inputs
                .into_iter()
                .map(|s| Input::Path(s.to_string()))
                .collect(),
            command: "echo test".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn test_task_no_inputs_always_affected() {
        let task = make_task(vec![]);
        let changed_files: Vec<PathBuf> = vec![];
        let root = Path::new(".");

        // Task with no inputs should always be affected
        assert!(task.is_affected_by(&changed_files, frame(root)));
    }

    #[test]
    fn test_task_with_inputs_matching() {
        let task = make_task(vec!["src/**"]);
        let changed_files = vec![PathBuf::from("src/lib.rs")];
        let root = Path::new(".");

        assert!(task.is_affected_by(&changed_files, frame(root)));
    }

    #[test]
    fn test_task_with_inputs_not_matching() {
        let task = make_task(vec!["src/**"]);
        let changed_files = vec![PathBuf::from("docs/readme.md")];
        let root = Path::new(".");

        assert!(!task.is_affected_by(&changed_files, frame(root)));
    }

    #[test]
    fn test_task_with_project_root_path_normalization() {
        let task = make_task(vec!["src/**"]);
        // File is repo-relative, but matches project-relative pattern
        let changed_files = vec![PathBuf::from("projects/website/src/app.rs")];
        let root = Path::new("projects/website");

        assert!(task.is_affected_by(&changed_files, frame(root)));
    }

    #[test]
    fn test_task_node_delegates_to_task() {
        let task = make_task(vec!["src/**"]);
        let node = TaskNode::Task(Box::new(task));
        let changed_files = vec![PathBuf::from("src/lib.rs")];
        let root = Path::new(".");

        assert!(node.is_affected_by(&changed_files, frame(root)));
    }

    #[test]
    fn test_task_group_any_affected() {
        let lint_task = make_task(vec!["src/**"]);
        let test_task = make_task(vec!["tests/**"]);

        let mut parallel_tasks = HashMap::new();
        parallel_tasks.insert("lint".to_string(), TaskNode::Task(Box::new(lint_task)));
        parallel_tasks.insert("test".to_string(), TaskNode::Task(Box::new(test_task)));

        let group = TaskGroup {
            type_: "group".to_string(),
            children: parallel_tasks,
            depends_on: vec![],
            max_concurrency: None,
            description: None,
        };

        // Change in src/ should affect the group (because lint is affected)
        let changed_files = vec![PathBuf::from("src/lib.rs")];
        let root = Path::new(".");

        assert!(group.is_affected_by(&changed_files, frame(root)));
    }

    #[test]
    fn test_task_group_none_affected() {
        let lint_task = make_task(vec!["src/**"]);
        let test_task = make_task(vec!["tests/**"]);

        let mut parallel_tasks = HashMap::new();
        parallel_tasks.insert("lint".to_string(), TaskNode::Task(Box::new(lint_task)));
        parallel_tasks.insert("test".to_string(), TaskNode::Task(Box::new(test_task)));

        let group = TaskGroup {
            type_: "group".to_string(),
            children: parallel_tasks,
            depends_on: vec![],
            max_concurrency: None,
            description: None,
        };

        // Change in docs/ should not affect the group
        let changed_files = vec![PathBuf::from("docs/readme.md")];
        let root = Path::new(".");

        assert!(!group.is_affected_by(&changed_files, frame(root)));
    }

    #[test]
    fn test_task_sequence_any_affected() {
        let build_task = make_task(vec!["src/**"]);
        let deploy_task = make_task(vec!["deploy/**"]);

        let sequence = TaskNode::Sequence(vec![
            TaskNode::Task(Box::new(build_task)),
            TaskNode::Task(Box::new(deploy_task)),
        ]);

        // Change in src/ should affect the sequence (because build is affected)
        let changed_files = vec![PathBuf::from("src/lib.rs")];
        let root = Path::new(".");

        assert!(sequence.is_affected_by(&changed_files, frame(root)));
    }

    #[test]
    fn test_input_patterns_returns_patterns() {
        let task = make_task(vec!["src/**", "Cargo.toml"]);
        let patterns = AffectedBy::input_patterns(&task);

        assert_eq!(patterns.len(), 2);
        assert!(patterns.contains(&InputPattern::Project("src/**")));
        assert!(patterns.contains(&InputPattern::Project("Cargo.toml")));
    }
}
