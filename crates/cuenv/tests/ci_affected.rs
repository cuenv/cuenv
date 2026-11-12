//! End-to-end contracts for which tasks `cuenv ci` selects (issue 611).
//!
//! Whatever cannot be shown to be unaffected runs. A run never reports "No
//! affected tasks" because an input lies outside the project, because a
//! dependency lives in a project `--path` leaves out, or because a pipeline
//! name is mistyped.

#[path = "../src/test_git.rs"]
mod test_git;

use std::error::Error;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

const APP_WITH_PARENT_INPUT: &str = r#"package cuenv
name: "app"
let _tasks = tasks
ci: pipelines: default: tasks: [_tasks.build]
tasks: build: {
	command: "sh"
	args: ["-c", "printf ran > ran"]
	inputs: ["src/**", "../content/articles/**"]
	hermetic: sandbox: "dir"
}
"#;

const CONTENT: &str = r#"package cuenv
name: "content"
let _tasks = tasks
ci: pipelines: default: tasks: [_tasks.articles]
tasks: articles: {
	command: "sh"
	args: ["-c", "mkdir -p out && cp articles/one.md out/one.md"]
	inputs: ["articles/**"]
	outputs: ["out/one.md"]
	hermetic: sandbox: "dir"
}
"#;

const DOCS_WITHOUT_CI: &str = r#"package cuenv
name: "docs"
tasks: lint: {command: "true", inputs: ["**/*.md"]}
"#;

/// A module of three projects in a repository of two commits. The second
/// commit changes only `content/articles/one.md`.
struct Monorepo {
    directory: TempDir,
    base: String,
}

impl Monorepo {
    fn new(app: &str) -> TestResult<Self> {
        let directory = tempfile::tempdir()?;
        let root = directory.path();
        write(root, "cue.mod/module.cue", MODULE)?;
        write(root, "app/env.cue", app)?;
        write(root, "app/src/main.ts", "export {}\n")?;
        write(root, "content/env.cue", CONTENT)?;
        write(root, "content/articles/one.md", "one\n")?;
        write(root, "docs/env.cue", DOCS_WITHOUT_CI)?;
        write(root, "docs/readme.md", "docs\n")?;
        test_git::init_repository(root)?;
        let base = test_git::commit_all(root, "base")?;
        write(root, "content/articles/one.md", "two\n")?;
        test_git::commit_all(root, "an article changes")?;
        Ok(Self { directory, base })
    }

    fn root(&self) -> &Path {
        self.directory.path()
    }

    fn ci(&self, arguments: &[&str]) -> TestResult<Output> {
        Ok(Command::new(env!("CARGO_BIN_EXE_cuenv"))
            .current_dir(self.root())
            .env_remove("GITHUB_ACTIONS")
            .env_remove("BUILDKITE")
            .env_remove("GITHUB_TOKEN")
            .env_remove("GH_TOKEN")
            .env("CUENV_STATE_DIR", self.root().join("state"))
            .arg("ci")
            .args(["--from", &self.base])
            .args(arguments)
            .output()?)
    }
}

const MODULE: &str = "module: \"test.example/monorepo\"\nlanguage: version: \"v0.14.0\"\n";

fn write(root: &Path, relative: &str, content: &str) -> TestResult {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, content)?;
    Ok(())
}

/// Everything a run printed, on one line: error reports are wrapped to the
/// terminal width, so a phrase may be split across lines.
fn text(output: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .replace('│', " ")
    .split_whitespace()
    .collect::<Vec<_>>()
    .join(" ")
}

fn assert_success(output: &Output) {
    assert!(output.status.success(), "{}", text(output));
}

#[test]
fn an_input_outside_the_project_runs_its_task_when_only_that_file_changed() -> TestResult {
    let repository = Monorepo::new(APP_WITH_PARENT_INPUT)?;

    let output = repository.ci(&["--path", "app"])?;

    assert_success(&output);
    assert!(repository.root().join("app/ran").exists());
    let printed = text(&output);
    assert!(!printed.contains("No affected tasks"), "{printed}");
    assert!(
        printed.contains("`..` inputs cannot be matched against changes"),
        "{printed}"
    );
    assert_eq!(
        printed
            .matches("has input '../content/articles/**'")
            .count(),
        1,
        "the warning is given once per task: {printed}"
    );
    Ok(())
}

#[test]
fn a_run_without_a_path_skips_projects_without_the_pipeline() -> TestResult {
    let repository = Monorepo::new(APP_WITH_PARENT_INPUT)?;

    let output = repository.ci(&[])?;

    // `docs` has no CI configuration. It is skipped, and `app` still runs.
    assert_success(&output);
    assert!(repository.root().join("app/ran").exists());
    assert!(repository.root().join("content/out/one.md").exists());
    Ok(())
}

#[test]
fn a_named_project_without_ci_is_still_an_error() -> TestResult {
    let repository = Monorepo::new(APP_WITH_PARENT_INPUT)?;

    let output = repository.ci(&["--path", "docs"])?;

    assert!(!output.status.success());
    assert!(
        text(&output).contains("has no CI configuration"),
        "{}",
        text(&output)
    );
    Ok(())
}

#[test]
fn a_pipeline_no_project_defines_is_an_error_naming_those_that_exist() -> TestResult {
    let repository = Monorepo::new(APP_WITH_PARENT_INPUT)?;

    for arguments in [
        vec!["--pipeline", "defualt"],
        vec!["--pipeline", "defualt", "--all"],
    ] {
        let output = repository.ci(&arguments)?;
        let printed = text(&output);
        assert!(!output.status.success(), "{printed}");
        assert!(printed.contains("pipeline 'defualt'"), "{printed}");
        assert!(
            printed.contains("Pipelines that exist: default"),
            "{printed}"
        );
    }
    assert!(!repository.root().join("app/ran").exists());
    Ok(())
}

#[test]
fn a_dependency_on_a_project_outside_the_path_is_resolved() -> TestResult {
    // The project-reference form of issue 611: `app` consumes an output of
    // `content`, only a file of `content` changed, and `--path app` leaves
    // `content` out of the run.
    let repository = Monorepo::new(
        r#"package cuenv
name: "app"
let _tasks = tasks
ci: pipelines: default: tasks: [_tasks.build]
tasks: build: {
	command: "sh"
	args: ["-c", "printf ran > ran"]
	inputs: ["src/**", {project: "content", task: "articles", map: [{from: "out/one.md", to: "vendor/one.md"}]}]
}
"#,
    )?;

    let output = repository.ci(&["--path", "app", "--dry-run"])?;

    assert_success(&output);
    let printed = text(&output);
    assert!(!printed.contains("No affected tasks"), "{printed}");
    assert!(!printed.contains("warning:"), "{printed}");
    Ok(())
}

#[test]
fn a_project_name_carried_by_a_child_directory_still_means_the_project() -> TestResult {
    // `content/drafts` is in the same package and declares no name, so it
    // inherits `content`. The dependency must resolve to `content` itself on
    // every run, whatever order the module's directories are visited in.
    let repository = Monorepo::new(
        r#"package cuenv
name: "app"
let _tasks = tasks
ci: pipelines: default: tasks: [_tasks.build]
tasks: build: {
	command: "sh"
	args: ["-c", "printf ran > ran"]
	inputs: ["src/**", {project: "content", task: "articles", map: [{from: "out/one.md", to: "vendor/one.md"}]}]
}
"#,
    )?;
    write(
        repository.root(),
        "content/drafts/env.cue",
        "package cuenv\ntasks: articles: inputs: [\"articles/**\"]\n",
    )?;

    for run in 0..12 {
        let output = repository.ci(&["--path", "app", "--dry-run"])?;
        assert_success(&output);
        let printed = text(&output);
        assert!(
            !printed.contains("No affected tasks"),
            "run {run}: {printed}"
        );
    }
    Ok(())
}

#[test]
fn a_base_that_cannot_be_compared_is_an_error() -> TestResult {
    let repository = Monorepo::new(APP_WITH_PARENT_INPUT)?;

    let output = Command::new(env!("CARGO_BIN_EXE_cuenv"))
        .current_dir(repository.root())
        .env_remove("GITHUB_ACTIONS")
        .env_remove("BUILDKITE")
        .env("CUENV_STATE_DIR", repository.root().join("state"))
        .args(["ci", "--from", "no-such-branch", "--dry-run"])
        .output()?;

    let printed = text(&output);
    assert!(!output.status.success(), "{printed}");
    assert!(printed.contains("--from no-such-branch"), "{printed}");
    assert!(printed.contains("was not found"), "{printed}");
    Ok(())
}
