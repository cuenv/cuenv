//! A comprehension in a task of a `#Project` must not break plain CUE or
//! cuenv's load, and a task must keep its identity when another one depends on
//! it.

use std::error::Error;
use std::fs;
use std::path::Path;

use cuenv::commands::CommandExecutor;
use cuenv_core::manifest::Project;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

const MODULE: &str = "github.com/cuenv/cuenv";

fn copy_schema(source: &Path, destination: &Path) -> TestResult {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let path = entry?.path();
        let target = destination.join(path.file_name().ok_or("schema entry has no name")?);
        if path.is_dir() {
            copy_schema(&path, &target)?;
        } else if path.extension().is_some_and(|extension| extension == "cue") {
            fs::copy(path, target)?;
        }
    }
    Ok(())
}

fn load(configuration: &str) -> TestResult<Project> {
    let directory = tempfile::tempdir()?;
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    copy_schema(&repository.join("schema"), &directory.path().join("schema"))?;
    fs::create_dir_all(directory.path().join("cue.mod"))?;
    fs::write(
        directory.path().join("cue.mod/module.cue"),
        format!("module: \"{MODULE}\"\nlanguage: version: \"v0.18.0\"\n"),
    )?;
    fs::write(directory.path().join("env.cue"), configuration)?;
    let path = directory.path().canonicalize()?;
    let executor = CommandExecutor::new("examples".to_string());
    executor.disable_autosync();
    let module = executor.module(&path)?;
    let instance = module
        .root_instance()
        .ok_or("the project was not evaluated")?;
    Ok(instance.to_project()?)
}

#[test]
fn an_if_or_for_in_a_task_loads() -> TestResult {
    let project = load(
        r#"package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
	name: "comprehensions"
	tasks: {
		conditional: schema.#Task & {command: "echo", if true {env: A: "1"}}
		looped: schema.#Task & {command: "echo", for key, value in {B: "2"} {env: (key): value}}
		scripted: schema.#Task & {script: "echo hi", if true {env: C: "3"}}
	}
}
"#,
    )?;
    let env = |name: &str| -> TestResult<Vec<String>> {
        let task = project
            .tasks
            .get(name)
            .and_then(|node| node.as_task())
            .ok_or_else(|| format!("{name} is not a task"))?;
        let mut keys = task.env.keys().cloned().collect::<Vec<_>>();
        keys.sort();
        Ok(keys)
    };
    assert_eq!(env("conditional")?, ["A"]);
    assert_eq!(env("looped")?, ["B"]);
    assert_eq!(env("scripted")?, ["C"]);
    Ok(())
}

#[test]
fn a_task_keeps_its_name_when_another_depends_on_it() -> TestResult {
    let project = load(
        r#"package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project
name: "identity"
let _t = tasks
tasks: {
	build: schema.#Task & {command: "echo", args: ["build"]}
	test: schema.#Task & {command: "echo", args: ["test"], dependsOn: [_t.build]}
}
"#,
    )?;
    let test = project.tasks.get("test").ok_or("missing test task")?;
    let dependencies = test
        .depends_on()
        .iter()
        .map(|dependency| dependency.task_name().to_string())
        .collect::<Vec<_>>();
    assert_eq!(dependencies, ["build"]);
    Ok(())
}
