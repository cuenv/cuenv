use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde_json::{Value, json};

use super::{Detected, Drift, Scan, ScriptDirectory, drift, into_tasks, package_of, render, scan};

fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn directory(path: &str) -> ScriptDirectory {
    ScriptDirectory {
        path: path.to_string(),
        ..ScriptDirectory::default()
    }
}

#[test]
fn each_script_file_is_found_by_name() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    write(&root.join("scripts/release.sh"), "#!/usr/bin/env bash\n");
    write(&root.join("scripts/report.py"), "print('x')\n");
    write(&root.join("scripts/deploy.ts"), "#!/usr/bin/env bun\n");
    write(&root.join("scripts/README.md"), "# Scripts\n");
    write(&root.join("scripts/.hidden.sh"), "#!/bin/sh\n");
    write(&root.join("scripts/helpers.lib.sh"), "#!/bin/sh\n");
    write(&root.join("scripts/nested/inner.sh"), "#!/bin/sh\n");
    let found = scan(
        root,
        &[ScriptDirectory {
            prefix: "s-".to_string(),
            exclude: vec!["*.lib.sh".to_string()],
            ..directory("scripts")
        }],
    );
    assert!(found.clashes.is_empty(), "{:?}", found.clashes);
    let names: Vec<&str> = found.scripts.keys().map(String::as_str).collect();
    assert_eq!(names, ["s-deploy", "s-release", "s-report"]);
    assert_eq!(found.scripts["s-release"].file, "scripts/release.sh");
    assert_eq!(
        found.scripts["s-report"].detected,
        Detected::Program("python3".to_string())
    );
}

#[test]
fn files_that_would_share_a_name_are_no_scripts_until_renamed() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    write(&root.join("scripts/make.sh"), "#!/bin/sh\n");
    write(&root.join("scripts/make.ts"), "#!/usr/bin/env bun\n");
    let clashing = scan(root, &[directory("scripts")]);
    assert!(clashing.scripts.is_empty());
    assert!(clashing.clashes[0].contains("`scripts/make.sh` and `scripts/make.ts`"));

    let renamed = scan(
        root,
        &[ScriptDirectory {
            rename: BTreeMap::from([("make.ts".to_string(), "makeTs".to_string())]),
            ..directory("scripts")
        }],
    );
    assert_eq!(renamed.clashes, Vec::<String>::new());
    let names: Vec<&str> = renamed.scripts.keys().map(String::as_str).collect();
    assert_eq!(names, ["make", "makeTs"]);
}

#[cfg(unix)]
#[test]
fn links_and_directories_outside_the_project_are_never_scripts() {
    let project = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let root = project.path();
    write(&outside.path().join("evil.sh"), "#!/bin/sh\n");
    write(&root.join("scripts/ok.sh"), "#!/bin/sh\n");
    write(&root.join("shared/linked.sh"), "#!/bin/sh\n");
    std::os::unix::fs::symlink(outside.path().join("evil.sh"), root.join("scripts/evil.sh"))
        .unwrap();
    std::os::unix::fs::symlink(
        root.join("shared/linked.sh"),
        root.join("scripts/linked.sh"),
    )
    .unwrap();
    std::os::unix::fs::symlink(outside.path(), root.join("away")).unwrap();
    let found = scan(
        root,
        &[
            directory("scripts"),
            directory("away"),
            directory("missing"),
        ],
    );
    let names: Vec<&str> = found.scripts.keys().map(String::as_str).collect();
    assert_eq!(names, ["linked", "ok"]);
}

#[test]
fn the_generated_file_lists_each_script_and_drifts_when_one_changes() {
    let project = tempfile::tempdir().unwrap();
    let root = project.path();
    write(&root.join("scripts/build.sh"), "#!/usr/bin/env bash\n");
    write(&root.join("scripts/lint.sh"), "echo lint\n");
    write(&root.join("scripts/serve.js"), "#!/usr/bin/env node\n");
    write(&root.join("scripts/make-docs.py"), "\n");
    let directories = [directory("scripts")];

    let (state, found) = drift(root, Some(&directories));
    assert_eq!(state, Drift::Stale, "no file yet");
    let text = render("cuenv", &found);
    assert!(text.contains("\npackage cuenv\n"));
    // Values aligned as `cue fmt` aligns them.
    assert!(text.ends_with(
        "scripts: {\n\
         \tbuild:       {file: \"scripts/build.sh\", interpreter: *\"bash\" | _}\n\
         \tlint:        {file: \"scripts/lint.sh\"}\n\
         \t\"make-docs\": {file: \"scripts/make-docs.py\", interpreter: *\"python3\" | _}\n\
         \tserve:       {file: \"scripts/serve.js\", unsupported: \"node\"}\n\
         }\n"
    ));
    fs::write(root.join(super::GENERATED_FILE), &text).unwrap();
    assert_eq!(drift(root, Some(&directories)).0, Drift::None);

    // The package clause is the caller's, never drift.
    fs::write(root.join(super::GENERATED_FILE), render("examples", &found)).unwrap();
    assert_eq!(drift(root, Some(&directories)).0, Drift::None);

    write(&root.join("scripts/deploy.sh"), "#!/bin/sh\n");
    assert_eq!(drift(root, Some(&directories)).0, Drift::Stale);
    assert_eq!(drift(root, None).0, Drift::Orphaned);
}

fn converted(project: Value) -> (Value, BTreeMap<String, Vec<String>>) {
    let mut value = project;
    let mut problems = BTreeMap::new();
    into_tasks(&mut value, "apps/web", &mut problems);
    (value, problems)
}

#[test]
fn each_script_runs_as_a_task_from_the_project() {
    let (value, problems) = converted(json!({
        "name": "web",
        "scriptDirs": [{"path": "scripts", "hermetic": true, "inputs": ["schema/**"], "outputs": ["gen/**"]}],
        "scripts": {
            "release": {
                "file": "scripts/release.sh",
                "interpreter": "bash",
                "dependsOn": [{"cuenvRef": {"kind": "task", "project": "web", "name": "build"}}],
                "params": {"positional": [{"description": "version", "required": true}]},
            },
            "gen": {"file": "scripts/gen.py", "interpreter": "python3", "inputs": ["only/**"]},
        },
    }));
    assert!(problems.is_empty(), "{problems:?}");
    let release = &value["tasks"]["release"];
    assert_eq!(release["command"], "bash");
    assert_eq!(release["args"], json!(["scripts/release.sh"]));
    assert_eq!(
        release["inputs"],
        json!(["scripts/release.sh", "schema/**"])
    );
    assert_eq!(release["outputs"], json!(["gen/**"]));
    assert_eq!(release["hermetic"], true);
    assert_eq!(
        release["dir"],
        json!({"from": "module", "path": "apps/web"})
    );
    assert_eq!(release["_source"]["file"], "apps/web/scripts/release.sh");
    assert_eq!(release["description"], "Script scripts/release.sh (bash)");
    assert_eq!(release["dependsOn"][0]["cuenvRef"]["name"], "build");
    assert_eq!(release["params"]["positional"][0]["required"], true);
    // A script's own inputs replace its directory's; the script stays first.
    assert_eq!(
        value["tasks"]["gen"]["inputs"],
        json!(["scripts/gen.py", "only/**"])
    );
    // Loading names the dependency by its stamp; the rest reads as a task.
    let mut readable = release.clone();
    readable.as_object_mut().unwrap().remove("dependsOn");
    let task: crate::tasks::Task = serde_json::from_value(readable).unwrap();
    assert_eq!(task.script_file.unwrap().interpreter, "bash");
}

#[test]
fn a_project_without_scripts_is_untouched() {
    let project = json!({"name": "web", "tasks": {"build": {"command": "make"}}});
    let (value, problems) = converted(project.clone());
    assert_eq!(value, project);
    assert!(problems.is_empty());
}

#[test]
fn scripts_that_cannot_run_or_clash_are_recorded_against_the_task() {
    let (value, problems) = converted(json!({
        "name": "app",
        "tasks": {"build": {"command": "make"}},
        "scripts": {
            "build": {"file": "scripts/build.sh", "interpreter": "sh"},
            "lint": {"file": "scripts/lint.sh"},
            "serve": {"file": "scripts/serve.js", "unsupported": "node"},
            "served": {"file": "scripts/served.js", "unsupported": "node", "interpreter": "bun"},
        },
    }));
    assert_eq!(value["tasks"]["build"]["command"], "make");
    assert!(problems["build"][0].contains("both a task and a script"));
    assert!(problems["lint"][0].contains("interpreter of `scripts/lint.sh` is ambiguous"));
    assert!(problems["serve"][0].contains("runs with `node`"));
    assert!(
        !problems.contains_key("served"),
        "an interpreter overrides the shebang"
    );
    assert_eq!(value["tasks"]["served"]["command"], "bun");
}

#[test]
fn a_rewrite_keeps_the_package_of_the_file() {
    let text = render("examples", &Scan::default());
    assert_eq!(package_of(&text), Some("examples"));
    assert_eq!(package_of("scripts: {}\n"), None);
}
