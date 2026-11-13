//! End-to-end contracts for file references (issue 611): a task reads files
//! of another project through that project's import,
//! `content.#Files & {paths: ["articles/**"]}`, and files anywhere in the
//! module through `schema.#ModuleFiles & {paths: [...]}`.
//!
//! The caller chooses the files and the other project declares nothing. A
//! change to one of those files selects the task in `cuenv ci`, the task's
//! cache key follows their content, and a path that matches no file is an
//! error for the task that reads it, never a silently smaller input set.

#[path = "../src/test_git.rs"]
mod test_git;

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

/// The module is the schema's own, so the fixture imports the real schema
/// copied into it. The bridge loads this language version.
const MODULE: &str = "module: \"github.com/cuenv/cuenv\"\nlanguage: version: \"v0.14.1\"\n";

/// `content`: holds articles it declares nothing about, and a task of its
/// own called like one of `app`'s.
const CONTENT: &str = r#"package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project
name: "content"
tasks: build: {
	command: "sh"
	args: ["-c", "printf content > built.txt"]
	inputs: ["articles/**"]
	outputs: ["built.txt"]
}
"#;

/// The #611 reproducer, rewritten: `app` reads the articles of `content`
/// through its import, and the lockfile at the module root.
const APP: &str = r#"package cuenv

import (
	"github.com/cuenv/cuenv/schema"
	content "github.com/cuenv/cuenv/content:cuenv"
)

schema.#Project
name: "app"
let _tasks = tasks
ci: pipelines: default: tasks: [_tasks.build]
tasks: build: {
	command: "sh"
	args: ["-c", "cat ../content/articles/one.md > seen.md"]
	inputs: [
		"src/**",
		content.#Files & {paths: ["articles/**"]},
		schema.#ModuleFiles & {paths: ["bun.lock"]},
	]
	outputs: ["seen.md"]
	hermetic: sandbox: "dir"
}
"#;

/// A module of two projects in a repository of two commits; the second
/// commit writes `changes`.
struct Repository {
    directory: TempDir,
    base: String,
}

impl Repository {
    fn new(app: &str, changes: &[(&str, &str)]) -> TestResult<Self> {
        Self::with(Fixture {
            app,
            files: &[],
            changes,
        })
    }

    fn with(fixture: Fixture<'_>) -> TestResult<Self> {
        let Fixture {
            app,
            files,
            changes,
        } = fixture;
        // CUE ignores directories whose name starts with a dot.
        let directory = tempfile::Builder::new().prefix("cuenv_test_").tempdir()?;
        let root = directory.path();
        write(root, "cue.mod/module.cue", MODULE)?;
        copy_schema(&root.join("schema"))?;
        write(root, "bun.lock", "lock one\n")?;
        write(root, "content/env.cue", CONTENT)?;
        write(root, "content/articles/one.md", "one\n")?;
        write(root, "app/env.cue", app)?;
        write(root, "app/src/main.ts", "export {}\n")?;
        write(root, "docs/readme.md", "docs\n")?;
        for (path, content) in files {
            write(root, path, content)?;
        }
        test_git::init_repository(root)?;
        let base = test_git::commit_all(root, "base")?;
        for (path, content) in changes {
            write(root, path, content)?;
        }
        test_git::commit_all(root, "a change")?;
        Ok(Self { directory, base })
    }

    fn root(&self) -> &Path {
        self.directory.path()
    }

    fn command(&self, directory: &Path) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cuenv"));
        command
            .current_dir(directory)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", std::env::var("HOME").unwrap_or_default())
            .env("USER", std::env::var("USER").unwrap_or_default())
            .env("CUENV_STATE_DIR", self.root().join("state"))
            .env("CUENV_CACHE_DIR", self.root().join("task-cache"));
        command
    }

    /// `cuenv ci`, comparing with the first commit.
    fn ci(&self, arguments: &[&str]) -> TestResult<Output> {
        Ok(self
            .command(self.root())
            .arg("ci")
            .args(["--from", &self.base])
            .args(arguments)
            .output()?)
    }

    /// `cuenv task` in one project of the module.
    fn task(&self, project: &str, arguments: &[&str]) -> TestResult<Output> {
        Ok(self
            .command(&self.root().join(project))
            .arg("task")
            .args(arguments)
            .output()?)
    }

    fn read(&self, relative: &str) -> TestResult<String> {
        let path = self.root().join(relative);
        fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()).into())
    }
}

/// What a [`Repository`] holds beyond the two projects.
#[derive(Clone, Copy)]
struct Fixture<'fixture> {
    /// The `env.cue` of `app`.
    app: &'fixture str,
    /// More files of the first commit.
    files: &'fixture [(&'fixture str, &'fixture str)],
    /// What the second commit writes.
    changes: &'fixture [(&'fixture str, &'fixture str)],
}

/// Copy the real schema packages into the fixture, so their imports
/// resolve.
fn copy_schema(destination: &Path) -> TestResult {
    copy_cue_files(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schema"),
        destination,
    )
}

fn copy_cue_files(source: &Path, destination: &Path) -> TestResult {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let path = entry?.path();
        let Some(name) = path.file_name() else {
            continue;
        };
        if path.is_dir() {
            copy_cue_files(&path, &destination.join(name))?;
        } else if path.extension().and_then(|extension| extension.to_str()) == Some("cue") {
            fs::copy(&path, destination.join(name))?;
        }
    }
    Ok(())
}

/// Write a file of the repository.
///
/// A project's tasks run with a hermetic `PATH` that names the usual system
/// directories, which hold no `cat` in the Nix build sandbox. Each project
/// the tests declare gets the test's own `PATH`.
fn write(root: &Path, relative: &str, content: &str) -> TestResult {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let declares_a_project = relative.ends_with("env.cue") && content.contains("\nname:");
    let content = if declares_a_project {
        let search_path = std::env::var("PATH").unwrap_or_default();
        format!(
            "{content}\nenv: PATH: {}\n",
            serde_json::to_string(&search_path)?
        )
    } else {
        content.to_string()
    };
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

fn assert_failure(output: &Output) {
    assert!(!output.status.success(), "{}", text(output));
}

#[test]
fn a_change_to_another_projects_file_selects_the_task_that_reads_it() -> TestResult {
    // Issue 611: only `content/articles/one.md` changed, and `--path app`
    // leaves `content` out of the run.
    let repository = Repository::new(APP, &[("content/articles/one.md", "two\n")])?;

    let output = repository.ci(&["--path", "app"])?;

    assert_success(&output);
    let printed = text(&output);
    assert!(!printed.contains("No affected tasks"), "{printed}");
    assert!(!printed.contains("warning:"), "{printed}");
    assert_eq!(repository.read("app/seen.md")?, "two\n");
    Ok(())
}

#[test]
fn a_change_to_a_module_file_selects_the_task_that_reads_it() -> TestResult {
    let repository = Repository::new(APP, &[("bun.lock", "lock two\n")])?;

    let output = repository.ci(&["--path", "app"])?;

    assert_success(&output);
    assert!(
        !text(&output).contains("No affected tasks"),
        "{}",
        text(&output)
    );
    assert_eq!(repository.read("app/seen.md")?, "one\n");
    Ok(())
}

#[test]
fn a_change_no_reference_names_selects_nothing() -> TestResult {
    // A file reference is an input like any other: the task is not run on
    // every change.
    let repository = Repository::new(APP, &[("docs/readme.md", "changed\n")])?;

    let output = repository.ci(&["--path", "app"])?;

    assert_success(&output);
    assert!(
        text(&output).contains("No affected tasks"),
        "{}",
        text(&output)
    );
    assert!(!repository.root().join("app/seen.md").exists());
    Ok(())
}

#[test]
fn a_rename_in_the_other_project_fails_the_reader_even_when_every_task_runs() -> TestResult {
    let repository = Repository::with(Fixture {
        app: APP,
        files: &[],
        changes: &[("content/posts/one.md", "one\n")],
    })?;
    fs::remove_dir_all(repository.root().join("content/articles"))?;

    for arguments in [vec!["--path", "app"], vec!["--path", "app", "--all"]] {
        let output = repository.ci(&arguments)?;

        assert_failure(&output);
        let printed = text(&output);
        assert!(
            printed.contains("`articles/**` matches nothing in content/"),
            "{printed}"
        );
        assert!(printed.contains("Pipeline 'default'"), "{printed}");
        assert!(printed.contains("task 'build'"), "{printed}");
    }
    assert!(!repository.root().join("app/seen.md").exists());
    Ok(())
}

#[test]
fn cuenv_task_stages_and_hashes_the_referenced_files() -> TestResult {
    let repository = Repository::new(APP, &[])?;

    let output = repository.task("app", &["build"])?;
    assert_success(&output);
    assert_eq!(repository.read("app/seen.md")?, "one\n");

    // The cache key follows the content of the referenced file.
    fs::write(repository.root().join("content/articles/one.md"), "two\n")?;
    let output = repository.task("app", &["build"])?;
    assert_success(&output);
    assert_eq!(repository.read("app/seen.md")?, "two\n");
    Ok(())
}

#[test]
fn cuenv_task_fails_the_reader_of_a_path_that_matches_nothing() -> TestResult {
    let app = APP.replace(r#"paths: ["articles/**"]"#, r#"paths: ["posts/**"]"#);
    let repository = Repository::new(&app, &[])?;

    let output = repository.task("app", &["build"])?;

    assert_failure(&output);
    let printed = text(&output);
    assert!(
        printed.contains("`posts/**` matches nothing in content/"),
        "{printed}"
    );
    assert!(printed.contains("task 'build' (app/env.cue:"), "{printed}");
    assert!(!repository.root().join("app/seen.md").exists());
    Ok(())
}

#[test]
fn a_path_that_leaves_its_anchor_is_rejected_with_the_rules() -> TestResult {
    let app = APP.replace(r#"paths: ["articles/**"]"#, r#"paths: ["../app/src/**"]"#);
    let repository = Repository::new(&app, &[])?;

    let output = repository.task("app", &["build"])?;

    assert_failure(&output);
    let printed = text(&output);
    assert!(printed.contains("inputs.1.paths.0"), "{printed}");
    assert!(
        printed.contains("for files outside the project use `schema.#ModuleFiles`"),
        "{printed}"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_reference_through_a_symbolic_link_is_an_error() -> TestResult {
    let app = APP.replace(r#"paths: ["articles/**"]"#, r#"paths: ["vendor/**"]"#);
    let repository = Repository::new(&app, &[])?;
    fs::create_dir_all(repository.root().join("shared"))?;
    fs::write(repository.root().join("shared/one.md"), "shared\n")?;
    std::os::unix::fs::symlink("../shared", repository.root().join("content/vendor"))?;

    let output = repository.task("app", &["build"])?;

    assert_failure(&output);
    let printed = text(&output);
    assert!(
        printed.contains("goes through the symbolic link `content/vendor`"),
        "{printed}"
    );
    Ok(())
}

/// `app` has a `build` of its own and depends on `content`'s `build`
/// through the import.
const APP_DEPENDING_ON_CONTENT: &str = r#"package cuenv

import (
	"github.com/cuenv/cuenv/schema"
	content "github.com/cuenv/cuenv/content:cuenv"
)

schema.#Project
name: "app"
let _tasks = tasks
ci: pipelines: {
	default: tasks: [_tasks.lint]
	release: tasks: [content.tasks.build]
}
tasks: {
	build: {
		command: "sh"
		args: ["-c", "printf local > local.txt"]
	}
	lint: {
		command: "sh"
		args: ["-c", "printf linted > linted.txt"]
		inputs: ["src/**"]
		outputs: ["linted.txt"]
	}
	deploy: {
		command: "sh"
		args: ["-c", "printf deployed > deployed.txt"]
		dependsOn: [content.tasks.build]
	}
}
"#;

#[test]
fn a_dependency_on_another_projects_task_is_a_diagnostic_not_a_binding() -> TestResult {
    let repository = Repository::new(APP_DEPENDING_ON_CONTENT, &[])?;

    let output = repository.task("app", &["deploy"])?;

    assert_failure(&output);
    let printed = text(&output);
    assert!(
        printed.contains("dependsOn[0] is task 'build' of project 'content'"),
        "{printed}"
    );
    assert!(
        printed.contains("content.#Files & {paths: [...]}"),
        "{printed}"
    );
    // The local `build` was not bound and did not run.
    assert!(!repository.root().join("app/local.txt").exists());
    assert!(!repository.root().join("app/deployed.txt").exists());

    // A task that does not reach it is unaffected.
    let output = repository.task("app", &["lint"])?;
    assert_success(&output);
    Ok(())
}

#[test]
fn another_projects_pipeline_entry_fails_only_its_pipeline() -> TestResult {
    let repository = Repository::new(APP_DEPENDING_ON_CONTENT, &[("app/src/main.ts", "x\n")])?;

    let output = repository.ci(&["--path", "app", "--pipeline", "release"])?;
    assert_failure(&output);
    let printed = text(&output);
    assert!(
        printed.contains("entry tasks[0] is task 'build' of project 'content'"),
        "{printed}"
    );
    assert!(
        printed.contains("not supported across projects yet"),
        "{printed}"
    );

    // The foreign entry of `release` does not fail `default`, nor does the
    // foreign dependency of a task `default` does not reach.
    let output = repository.ci(&["--path", "app"])?;
    assert_success(&output);
    assert_eq!(repository.read("app/linted.txt")?, "linted");

    // A run without a path plans every project, and `release` reaches the
    // entry: the run fails, naming it.
    let output = repository.ci(&["--pipeline", "release"])?;
    assert_failure(&output);
    assert!(
        text(&output).contains("not supported across projects yet"),
        "{}",
        text(&output)
    );
    Ok(())
}

#[test]
fn a_reference_without_paths_fails_its_task_and_the_rest_of_the_project_loads() -> TestResult {
    // `content.#Files` alone is incomplete. `cue export` rejects it; the
    // evaluation cuenv uses lets it through.
    let app = APP.replace(
        r#"content.#Files & {paths: ["articles/**"]},"#,
        "content.#Files,",
    );
    let app = format!(
        "{app}\ntasks: other: {{command: \"sh\", args: [\"-c\", \"printf other > other.txt\"], \
         outputs: [\"other.txt\"]}}\n"
    );
    let repository = Repository::new(&app, &[])?;

    let output = repository.task("app", &["build"])?;
    assert_failure(&output);
    let printed = text(&output);
    assert!(printed.contains("names no files: say which"), "{printed}");
    assert!(
        printed.contains("content.#Files & {paths: [...]}"),
        "{printed}"
    );

    let output = repository.task("app", &["other"])?;
    assert_success(&output);
    assert_eq!(repository.read("app/other.txt")?, "other");
    Ok(())
}
