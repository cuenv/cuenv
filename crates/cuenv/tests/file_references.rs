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

    /// `cuenv sync ci` at the root of the repository.
    fn sync_ci(&self) -> TestResult<Output> {
        Ok(self.command(self.root()).args(["sync", "ci"]).output()?)
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

#[test]
fn a_path_into_the_repository_metadata_is_rejected_with_the_rules() -> TestResult {
    for path in [".git/config", "articles/.GIT/HEAD"] {
        let app = APP.replace(
            r#"paths: ["articles/**"]"#,
            &format!(r#"paths: ["{path}"]"#),
        );
        let repository = Repository::new(&app, &[])?;

        let output = repository.task("app", &["build"])?;

        assert_failure(&output);
        let printed = text(&output);
        assert!(printed.contains("inputs.1.paths.0"), "{printed}");
        assert!(printed.contains("`.git` or empty segment"), "{printed}");
        assert!(!repository.root().join("app/seen.md").exists(), "{printed}");
    }
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
		outputs: ["deployed.txt"]
	}
}
"#;

#[test]
fn a_dependency_on_another_projects_task_binds_to_that_task_never_a_local_one() -> TestResult {
    let repository = Repository::new(APP_DEPENDING_ON_CONTENT, &[])?;

    let output = repository.task("app", &["deploy"])?;

    assert_success(&output);
    // `content`'s build ran, in `content`; the local `build` was not bound.
    assert!(repository.root().join("content/built.txt").exists());
    assert!(!repository.root().join("app/local.txt").exists());
    assert_eq!(repository.read("app/deployed.txt")?, "deployed");
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

/// A directory of the module that does not evaluate.
const BROKEN: &str = r#"package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project
name: "broken"
conflict: 1
conflict: 2
"#;

#[test]
fn an_unrelated_directory_that_does_not_evaluate_does_not_fail_the_reader() -> TestResult {
    // The name of `content` is read from what `app` imports; no other
    // directory of the module is evaluated to bind it.
    let repository = Repository::with(Fixture {
        app: APP,
        files: &[("broken/env.cue", BROKEN)],
        changes: &[],
    })?;

    let output = repository.task("app", &["build"])?;

    assert_success(&output);
    assert_eq!(repository.read("app/seen.md")?, "one\n");
    Ok(())
}

/// `content/drafts` holds no project of its own: it inherits `content`'s
/// `name` from the directory above.
const DRAFTS: &str = "package cuenv\n\ndrafts: true\n";

#[test]
fn a_reference_through_a_directory_that_only_inherits_the_name_is_rejected() -> TestResult {
    // `drafts.#Files` is stamped `content`, as `content.#Files` is: the
    // directory it was imported from is what is wrong.
    let app = APP
        .replace(
            r#"content "github.com/cuenv/cuenv/content:cuenv""#,
            r#"content "github.com/cuenv/cuenv/content/drafts:cuenv""#,
        )
        .replace(r#"paths: ["articles/**"]"#, r#"paths: ["**"]"#);
    let repository = Repository::with(Fixture {
        app: &app,
        files: &[
            ("content/drafts/env.cue", DRAFTS),
            ("content/drafts/a.md", "a\n"),
        ],
        changes: &[("content/articles/one.md", "two\n")],
    })?;

    for output in [
        repository.task("app", &["build"])?,
        repository.ci(&["--path", "app"])?,
    ] {
        assert_failure(&output);
        let printed = text(&output);
        // Reports are wrapped to the terminal, so a path may be split.
        assert!(
            printed.contains("(imported by this project as"),
            "{printed}"
        );
        assert!(
            printed.contains("only inherits project 'content' from `content`"),
            "{printed}"
        );
        assert!(
            printed.contains("import the project's own directory"),
            "{printed}"
        );
    }
    Ok(())
}

#[test]
fn a_reference_to_a_project_that_is_not_imported_does_not_bind() -> TestResult {
    // Written by hand: no import stamped it, so no project is bound by its
    // name, even though the module has one called `content`.
    let app = APP.replace(
        r#"content.#Files & {paths: ["articles/**"]}"#,
        r#"schema.#ProjectFiles & {cuenvProject: "content", paths: ["articles/**"]}"#,
    );
    let app = app.replace("\tcontent \"github.com/cuenv/cuenv/content:cuenv\"\n", "");
    let repository = Repository::new(&app, &[])?;

    let output = repository.task("app", &["build"])?;

    assert_failure(&output);
    let printed = text(&output);
    assert!(
        printed.contains("project 'content' is not imported here"),
        "{printed}"
    );
    assert!(
        printed.contains(r#"`content "github.com/cuenv/cuenv/<directory of content>:cuenv"`"#),
        "{printed}"
    );
    Ok(())
}

#[test]
fn a_reference_into_a_nested_repository_is_an_error_and_a_glob_leaves_one_out() -> TestResult {
    // `content/themes` is a submodule: a directory holding `.git`.
    let files: &[(&str, &str)] = &[
        ("content/themes/.git", "gitdir: ../../.git/modules/themes\n"),
        ("content/themes/theme.css", "body {}\n"),
    ];
    let into_submodule = APP.replace(r#"paths: ["articles/**"]"#, r#"paths: ["themes/**"]"#);
    let repository = Repository::with(Fixture {
        app: &into_submodule,
        files,
        changes: &[],
    })?;

    let output = repository.task("app", &["build"])?;

    assert_failure(&output);
    let printed = text(&output);
    assert!(
        printed.contains("reaches into the nested repository"),
        "{printed}"
    );

    // Under a glob the submodule's files are left out of the inputs, so a
    // sandboxed task does not see them.
    let over_everything = APP
        .replace(r#"paths: ["articles/**"]"#, r#"paths: ["**"]"#)
        .replace(
            "cat ../content/articles/one.md > seen.md",
            "test ! -e ../content/themes/theme.css && cat ../content/articles/one.md > seen.md",
        );
    let repository = Repository::with(Fixture {
        app: &over_everything,
        files,
        changes: &[],
    })?;

    let output = repository.task("app", &["build"])?;

    assert_success(&output);
    assert_eq!(repository.read("app/seen.md")?, "one\n");
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_dry_run_binds_references_and_reports_a_link() -> TestResult {
    let through_link = APP.replace(r#"paths: ["articles/**"]"#, r#"paths: ["vendor/**"]"#);
    let repository = Repository::new(&through_link, &[("content/articles/one.md", "two\n")])?;
    fs::create_dir_all(repository.root().join("shared"))?;
    fs::write(repository.root().join("shared/one.md"), "shared\n")?;
    std::os::unix::fs::symlink("../shared", repository.root().join("content/vendor"))?;

    let output = repository.ci(&["--path", "app", "--dry-run"])?;

    assert_failure(&output);
    let printed = text(&output);
    assert!(
        printed.contains("goes through the symbolic link `content/vendor`"),
        "{printed}"
    );

    // A glob over the same directory leaves the link out: the dry run
    // plans, and selects the task for the changed article.
    let over_everything = APP.replace(r#"paths: ["articles/**"]"#, r#"paths: ["**"]"#);
    let repository = Repository::new(&over_everything, &[("content/articles/one.md", "two\n")])?;
    std::os::unix::fs::symlink("../shared", repository.root().join("content/vendor"))?;

    let output = repository.ci(&["--path", "app", "--dry-run"])?;

    assert_success(&output);
    assert!(
        !text(&output).contains("No affected tasks"),
        "{}",
        text(&output)
    );
    Ok(())
}

/// `app` with one pipeline per workflow mode, each of one task that reads
/// its own files, another project's and the module's.
const APP_WITH_WORKFLOWS: &str = r#"package cuenv

import (
	"github.com/cuenv/cuenv/schema"
	content "github.com/cuenv/cuenv/content:cuenv"
)

schema.#Project
name: "app"
let _tasks = tasks
ci: {
	providers: ["github"]
	pipelines: {
		thin: {
			mode: "thin"
			when: branch: "main"
			tasks: [_tasks.build]
		}
		expanded: {
			mode: "expanded"
			when: branch: "main"
			tasks: [_tasks.build]
		}
		matrix: {
			when: branch: "main"
			tasks: [{type: "matrix", task: _tasks.build, matrix: arch: ["x86_64", "aarch64"]}]
		}
	}
}
tasks: build: {
	command: "true"
	inputs: [
		"src/**",
		content.#Files & {paths: ["articles/**", "*.md"]},
		schema.#ModuleFiles & {paths: ["bun.lock"]},
	]
}
"#;

/// The `paths` filter of the `push` trigger of a generated workflow.
fn push_paths(workflow: &str) -> Vec<String> {
    let mut lines = workflow.lines().skip_while(|line| line.trim() != "push:");
    let Some(push) = lines.next() else {
        return Vec::new();
    };
    let indent = push.len() - push.trim_start().len();
    lines
        .take_while(|line| line.len() - line.trim_start().len() > indent)
        .skip_while(|line| line.trim() != "paths:")
        .skip(1)
        .map_while(|line| line.trim().strip_prefix("- "))
        .map(|path| path.trim_matches(['\'', '"']).to_string())
        .collect()
}

#[test]
fn generated_workflows_trigger_on_the_files_a_reference_names_in_every_mode() -> TestResult {
    let repository = Repository::new(APP_WITH_WORKFLOWS, &[])?;

    let output = repository.sync_ci()?;

    assert_success(&output);
    // The golden: what each mode's workflow starts for, beyond the CUE the
    // project is built from.
    let golden = [
        "app/src/**",
        "bun.lock",
        "bun.lock/**",
        "content/**",
        "content/articles/**",
    ];
    for mode in ["thin", "expanded", "matrix"] {
        let workflow = repository.read(&format!(".github/workflows/app-{mode}.yml"))?;
        let paths = push_paths(&workflow);
        let from_inputs: Vec<&str> = paths
            .iter()
            .map(String::as_str)
            // The CUE the project is built from, and the workflow itself.
            .filter(|path| {
                !path.ends_with("*.cue")
                    && !path.starts_with("cue.mod")
                    && !path.starts_with(".github/")
            })
            .collect();
        assert_eq!(from_inputs, golden, "{mode}:\n{workflow}");
    }

    // An export binds the references the same way.
    let output = repository.ci(&["--export", "buildkite", "--pipeline", "thin"])?;
    assert_success(&output);
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("steps:"),
        "{}",
        text(&output)
    );

    // A rename in `content` does not concern workflow generation, which does
    // not read the working tree.
    fs::remove_dir_all(repository.root().join("content/articles"))?;
    let output = repository.sync_ci()?;
    assert_success(&output);
    Ok(())
}

#[test]
fn workflow_generation_rejects_a_reference_that_does_not_bind() -> TestResult {
    let unbound = APP_WITH_WORKFLOWS
        .replace(
            r#"content.#Files & {paths: ["articles/**", "*.md"]}"#,
            r#"schema.#ProjectFiles & {cuenvProject: "elsewhere", paths: ["a/**"]}"#,
        )
        .replace("\tcontent \"github.com/cuenv/cuenv/content:cuenv\"\n", "");
    let repository = Repository::new(&unbound, &[])?;

    let output = repository.sync_ci()?;

    assert_failure(&output);
    let printed = text(&output);
    assert!(
        printed.contains("project 'elsewhere' is not imported here"),
        "{printed}"
    );
    Ok(())
}

/// A project of another CUE package than the one cuenv evaluates.
const OTHER_PACKAGE: &str = r#"package other

import "github.com/cuenv/cuenv/schema"

schema.#Project
name: "other"
"#;

#[test]
fn a_reference_to_a_project_of_another_package_does_not_bind() -> TestResult {
    let app = APP
        .replace(
            r#"content "github.com/cuenv/cuenv/content:cuenv""#,
            r#"content "github.com/cuenv/cuenv/other:other""#,
        )
        .replace(r#"paths: ["articles/**"]"#, r#"paths: ["notes.md"]"#);
    let repository = Repository::with(Fixture {
        app: &app,
        files: &[
            ("other/env.cue", OTHER_PACKAGE),
            ("other/notes.md", "notes\n"),
        ],
        changes: &[],
    })?;

    let output = repository.task("app", &["build"])?;

    assert_failure(&output);
    let printed = text(&output);
    assert!(
        printed.contains("`other` declares package `other`; cuenv projects use package `cuenv`"),
        "{printed}"
    );
    Ok(())
}
