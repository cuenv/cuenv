//! A sequence that holds or aliases another sequence must evaluate: the same
//! task then appears under two names, which used to fail the whole project.
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
fn a_sequence_can_hold_or_alias_another_sequence() -> TestResult {
    let project = load(
        r#"package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project
name: "sequences"
let _t = tasks
tasks: {
	build: schema.#Task & {command: "echo", args: ["build"], hermetic: false}
	checks: [_t.build, _t.build]
	release: [_t.checks, _t.build]
	alias: _t.checks
}
"#,
    )?;
    let mut names = project.tasks.keys().map(String::as_str).collect::<Vec<_>>();
    names.sort_unstable();
    assert_eq!(names, ["alias", "build", "checks", "release"]);
    Ok(())
}
