use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use serde_json::{Value, json};

use super::{Interpreter, ScriptPlace, discover, interpreter, shebang_program};

fn program(name: &str) -> Interpreter {
    Interpreter::Program(name.to_string())
}

#[test]
fn shebangs_name_their_program_through_env_and_its_options() {
    assert_eq!(shebang_program("#!/bin/bash"), Some("bash".to_string()));
    assert_eq!(
        shebang_program("#!/usr/bin/env bash"),
        Some("bash".to_string())
    );
    assert_eq!(
        shebang_program("#!/usr/bin/env -S bun run"),
        Some("bun".to_string())
    );
    assert_eq!(
        shebang_program("#!/usr/bin/env -S LANG=C python3 -u"),
        Some("python3".to_string())
    );
    assert_eq!(shebang_program("echo hi"), None);
    assert_eq!(shebang_program("#!"), None);
}

#[test]
fn an_override_wins_then_the_shebang_then_an_unambiguous_extension() {
    assert_eq!(
        interpreter("x.sh", "#!/bin/bash", Some("fish")),
        Some(program("fish"))
    );
    assert_eq!(
        interpreter("x.py", "#!/usr/bin/env python3.12", None),
        Some(program("python3.12"))
    );
    assert_eq!(
        interpreter("x.ts", "#!/usr/bin/env bun", None),
        Some(program("bun"))
    );
    assert_eq!(interpreter("x.py", "", None), Some(program("python3")));
    assert_eq!(interpreter("x.nu", "", None), Some(program("nu")));
    assert_eq!(interpreter("x.fish", "", None), Some(program("fish")));
    assert_eq!(interpreter("x.bash", "", None), Some(program("bash")));
    assert_eq!(interpreter("x", "#!/bin/sh", None), Some(program("sh")));
    // JavaScript and TypeScript are never assumed to be Bun, nor `.sh` bash.
    for file in ["x.ts", "x.js", "x.sh"] {
        assert!(
            matches!(interpreter(file, "", None), Some(Interpreter::Ambiguous(_))),
            "{file}"
        );
    }
    assert_eq!(
        interpreter("x.js", "#!/usr/bin/env node", None),
        Some(Interpreter::Unsupported("node".to_string()))
    );
    assert_eq!(
        interpreter("x.sh", "", Some("zsh")),
        Some(Interpreter::Unsupported("zsh".to_string()))
    );
    // A file with no sign of being a script is no task.
    assert_eq!(interpreter("README.md", "# Scripts", None), None);
    assert_eq!(interpreter("Makefile", "all:", None), None);
}

fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn discovered(
    module: &Path,
    instance: &Path,
    project: Value,
) -> (Value, BTreeMap<String, Vec<String>>) {
    let mut value = project;
    let mut problems = BTreeMap::new();
    discover(
        &mut value,
        ScriptPlace {
            module_root: module,
            instance,
        },
        &mut problems,
    );
    (value, problems)
}

#[test]
fn each_script_file_becomes_a_task_that_runs_it_from_the_project() {
    let module = tempfile::tempdir().unwrap();
    let project = module.path().join("apps/web");
    write(
        &project.join("scripts/release.sh"),
        "#!/usr/bin/env bash\necho release\n",
    );
    write(&project.join("scripts/report.py"), "print('x')\n");
    write(&project.join("scripts/deploy.ts"), "#!/usr/bin/env bun\n");
    write(&project.join("scripts/README.md"), "# Scripts\n");
    write(&project.join("scripts/.hidden.sh"), "#!/bin/sh\n");
    write(&project.join("scripts/helpers.lib.sh"), "#!/bin/sh\n");
    write(&project.join("scripts/nested/inner.sh"), "#!/bin/sh\n");
    let (value, problems) = discovered(
        module.path(),
        Path::new("apps/web"),
        json!({
            "name": "web",
            "taskScripts": [{"path": "scripts", "prefix": "s-", "exclude": ["*.lib.sh"]}],
        }),
    );
    assert!(problems.is_empty(), "{problems:?}");
    let tasks = value["tasks"].as_object().unwrap();
    let names: Vec<&str> = tasks.keys().map(String::as_str).collect();
    assert_eq!(names, ["s-deploy", "s-release", "s-report"]);
    let release = &tasks["s-release"];
    assert_eq!(release["command"], "bash");
    assert_eq!(release["args"], json!(["scripts/release.sh"]));
    assert_eq!(release["inputs"], json!(["scripts/release.sh"]));
    assert_eq!(release["hermetic"], false);
    assert_eq!(
        release["dir"],
        json!({"from": "module", "path": "apps/web"})
    );
    assert_eq!(release["_source"]["file"], "apps/web/scripts/release.sh");
    assert_eq!(release["description"], "Script scripts/release.sh (bash)");
    assert_eq!(
        release["_cuenvScript"],
        json!({"path": "scripts/release.sh", "interpreter": "bash"})
    );
    assert_eq!(tasks["s-report"]["command"], "python3");
    assert_eq!(tasks["s-deploy"]["command"], "bun");
    let task: crate::tasks::Task = serde_json::from_value(release.clone()).unwrap();
    assert_eq!(task.script_file.unwrap().interpreter, "bash");
}

#[test]
fn a_project_without_task_scripts_is_untouched() {
    let module = tempfile::tempdir().unwrap();
    let project = json!({"name": "web", "tasks": {"build": {"command": "make"}}});
    let (value, problems) = discovered(module.path(), Path::new(""), project.clone());
    assert_eq!(value, project);
    assert!(problems.is_empty());
}

#[test]
fn collisions_and_scripts_that_cannot_run_are_recorded_against_the_task() {
    let module = tempfile::tempdir().unwrap();
    let root = module.path();
    write(&root.join("scripts/build.sh"), "#!/bin/sh\n");
    write(&root.join("scripts/release.bash"), "echo\n");
    write(&root.join("scripts/release.sh"), "#!/bin/bash\n");
    write(&root.join("scripts/lint.sh"), "echo lint\n");
    write(&root.join("scripts/serve.js"), "#!/usr/bin/env node\n");
    let (value, problems) = discovered(
        root,
        Path::new(""),
        json!({
            "name": "app",
            "tasks": {"build": {"command": "make"}},
            "taskScripts": [{"path": "scripts"}],
        }),
    );
    let tasks = value["tasks"].as_object().unwrap();
    // The declared task stays, and says why it cannot run.
    assert_eq!(tasks["build"]["command"], "make");
    assert!(
        problems["build"][0]
            .contains("declared in CUE and also discovered from `scripts/build.sh`")
    );
    // Two files, one name: the first by file name is kept and the clash is named.
    assert_eq!(tasks["release"]["args"], json!(["scripts/release.bash"]));
    assert!(problems["release"][0].contains("`scripts/release.bash` and `scripts/release.sh`"));
    assert!(problems["lint"][0].contains("interpreter of `scripts/lint.sh` is ambiguous"));
    assert!(problems["serve"][0].contains("runs with `node`"));
    assert_eq!(dir_of(&tasks["lint"]), ".");
}

fn dir_of(task: &Value) -> &str {
    task["dir"]["path"].as_str().unwrap()
}

#[cfg(unix)]
#[test]
fn links_and_directories_outside_the_project_are_never_tasks() {
    let module = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let root = module.path();
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
    let (value, problems) = discovered(
        root,
        Path::new(""),
        json!({
            "name": "app",
            "taskScripts": [{"path": "scripts"}, {"path": "away"}, {"path": "missing"}],
        }),
    );
    assert!(problems.is_empty(), "{problems:?}");
    let names: Vec<&String> = value["tasks"].as_object().unwrap().keys().collect();
    assert_eq!(names, ["linked", "ok"]);
}

#[test]
fn a_directory_can_opt_into_hermetic_execution_with_its_own_inputs() {
    let module = tempfile::tempdir().unwrap();
    write(&module.path().join("scripts/gen.py"), "\n");
    let (value, _) = discovered(
        module.path(),
        Path::new(""),
        json!({
            "name": "app",
            "taskScripts": [{
                "path": "scripts",
                "hermetic": true,
                "inputs": ["schema/**"],
                "outputs": ["generated/**"],
                "interpreters": {"gen.py": "python"},
            }],
        }),
    );
    let task = &value["tasks"]["gen"];
    assert_eq!(task["hermetic"], true);
    assert_eq!(task["inputs"], json!(["scripts/gen.py", "schema/**"]));
    assert_eq!(task["outputs"], json!(["generated/**"]));
    assert_eq!(task["command"], "python");
}
