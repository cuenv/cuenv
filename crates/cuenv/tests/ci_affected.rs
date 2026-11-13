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

/// What the second commit of a [`Monorepo`] writes unless a test says
/// otherwise: one article of `content`.
const AN_ARTICLE_CHANGES: &[(&str, &str)] = &[("content/articles/one.md", "two\n")];

/// `app` consumes the article `content` renders, at the path it maps it to.
const APP_CONSUMING_CONTENT: &str = r#"package cuenv
name: "app"
let _tasks = tasks
ci: pipelines: default: tasks: [_tasks.build]
tasks: build: {
	command: "sh"
	args: ["-c", "cp vendor/one.md seen.md"]
	inputs: ["src/**", {project: "content", task: "articles", map: [{from: "out/one.md", to: "vendor/one.md"}]}]
	outputs: ["seen.md"]
}
"#;

/// A module of three projects in a repository of two commits. The second
/// commit changes only `content/articles/one.md`.
/// How a [`Monorepo`] is laid out.
#[derive(Clone, Copy)]
struct Layout<'layout> {
    /// Where the checkout is beneath the temporary directory.
    checkout: &'layout Path,
    /// The `env.cue` of the project `app`.
    app: &'layout str,
    /// More files of the first commit.
    files: &'layout [(&'layout str, &'layout str)],
    /// What the second commit writes.
    changes: &'layout [(&'layout str, &'layout str)],
}

struct Monorepo {
    directory: TempDir,
    /// Where the checkout is beneath the temporary directory.
    checkout: std::path::PathBuf,
    base: String,
}

impl Monorepo {
    fn new(app: &str) -> TestResult<Self> {
        Self::changing(app, &[], AN_ARTICLE_CHANGES)
    }

    /// A monorepo with some more files in its first commit, whose second
    /// commit writes `changes`.
    fn changing(app: &str, files: &[(&str, &str)], changes: &[(&str, &str)]) -> TestResult<Self> {
        Self::laid_out(&Layout {
            checkout: Path::new(""),
            app,
            files,
            changes,
        })
    }

    /// A monorepo laid out as described, in a new temporary directory.
    fn laid_out(layout: &Layout<'_>) -> TestResult<Self> {
        let Layout {
            checkout,
            app,
            files,
            changes,
        } = *layout;
        let directory = tempfile::tempdir()?;
        let root = directory.path().join(checkout);
        fs::create_dir_all(&root)?;
        write(&root, "cue.mod/module.cue", MODULE)?;
        write(&root, "app/env.cue", app)?;
        write(&root, "app/src/main.ts", "export {}\n")?;
        write(&root, "content/env.cue", CONTENT)?;
        write(&root, "content/articles/one.md", "one\n")?;
        write(&root, "docs/env.cue", DOCS_WITHOUT_CI)?;
        write(&root, "docs/readme.md", "docs\n")?;
        for (path, content) in files {
            write(&root, path, content)?;
        }
        test_git::init_repository(&root)?;
        let base = test_git::commit_all(&root, "base")?;
        for (path, content) in changes {
            write(&root, path, content)?;
        }
        test_git::commit_all(&root, "a change")?;
        Ok(Self {
            directory,
            checkout: checkout.to_path_buf(),
            base,
        })
    }

    fn root(&self) -> std::path::PathBuf {
        self.directory.path().join(&self.checkout)
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

    /// Run `cuenv task` in one project of the module.
    fn task(&self, project: &str, arguments: &[&str]) -> TestResult<Output> {
        Ok(Command::new(env!("CARGO_BIN_EXE_cuenv"))
            .current_dir(self.root().join(project))
            .env_remove("GITHUB_ACTIONS")
            .env_remove("BUILDKITE")
            .env("CUENV_CACHE_DIR", self.root().join("task-cache"))
            .arg("task")
            .args(arguments)
            .output()?)
    }

    fn read(&self, relative: &str) -> TestResult<String> {
        let path = self.root().join(relative);
        fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()).into())
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
    let repository = Monorepo::new(APP_CONSUMING_CONTENT)?;

    let output = repository.ci(&["--path", "app"])?;

    assert_success(&output);
    let printed = text(&output);
    assert!(!printed.contains("No affected tasks"), "{printed}");
    assert!(!printed.contains("warning:"), "{printed}");
    // The producer ran first, in its own project, on the changed article.
    assert_eq!(repository.read("content/out/one.md")?, "two\n");
    // The consumer read it at the path it mapped it to, which is staged for
    // the task and never written into the checkout.
    assert_eq!(repository.read("app/seen.md")?, "two\n");
    assert!(!repository.root().join("app/vendor").exists());
    Ok(())
}

#[test]
fn a_run_without_a_path_runs_the_dependency_in_its_own_project() -> TestResult {
    let repository = Monorepo::new(APP_CONSUMING_CONTENT)?;

    let output = repository.ci(&[])?;

    assert_success(&output);
    let printed = text(&output);
    assert!(!printed.contains("warning:"), "{printed}");
    assert_eq!(repository.read("content/out/one.md")?, "two\n");
    assert_eq!(repository.read("app/seen.md")?, "two\n");
    Ok(())
}

#[test]
fn cuenv_task_runs_the_same_dependency() -> TestResult {
    let repository = Monorepo::new(APP_CONSUMING_CONTENT)?;

    let output = repository.task("app", &["build"])?;

    assert_success(&output);
    assert_eq!(repository.read("content/out/one.md")?, "two\n");
    assert_eq!(repository.read("app/seen.md")?, "two\n");
    assert!(!repository.root().join("app/vendor").exists());
    Ok(())
}

/// `content`, with a hook and a variable of its own. Its task records what
/// it could see of them, and of `app`'s.
const CONTENT_WITH_HOOK: &str = r#"package cuenv
name: "content"
env: CONTENT_VALUE: "content-own"
hooks: onEnter: mark: {
	command: "sh"
	args: ["-c", "echo 'export CONTENT_HOOK=from-content-hook'"]
	source: true
}
let _tasks = tasks
ci: pipelines: default: tasks: [_tasks.articles]
tasks: articles: {
	command: "sh"
	args: ["-c", "mkdir -p out && cp articles/one.md out/one.md && printf '%s|%s|%s|%s' \"${CONTENT_VALUE:-unset}\" \"${CONTENT_HOOK:-unset}\" \"${APP_SECRET:-unset}\" \"${APP_HOOK:-unset}\" > out/env.txt"]
	inputs: ["articles/**"]
	outputs: ["out/one.md", "out/env.txt"]
}
"#;

/// `app`, with a hook and a secret of its own.
const APP_WITH_HOOK: &str = r#"package cuenv
name: "app"
env: APP_SECRET: {resolver: "exec", command: "echo", args: ["app-only"]}
hooks: onEnter: mark: {
	command: "sh"
	args: ["-c", "echo 'export APP_HOOK=from-app-hook'"]
	source: true
}
let _tasks = tasks
ci: pipelines: default: tasks: [_tasks.build]
tasks: build: {
	command: "sh"
	args: ["-c", "cp vendor/one.md seen.md && printf '%s|%s|%s|%s' \"${APP_SECRET:-unset}\" \"${APP_HOOK:-unset}\" \"${CONTENT_VALUE:-unset}\" \"${CONTENT_HOOK:-unset}\" > env.txt"]
	inputs: ["src/**", {project: "content", task: "articles", map: [{from: "out/one.md", to: "vendor/one.md"}]}]
	outputs: ["seen.md", "env.txt"]
}
"#;

#[test]
fn a_dependency_runs_with_its_own_hooks_and_never_the_consumers_secrets() -> TestResult {
    let repository = Monorepo::changing(
        APP_WITH_HOOK,
        &[("content/env.cue", CONTENT_WITH_HOOK)],
        AN_ARTICLE_CHANGES,
    )?;

    let output = repository.ci(&["--path", "app"])?;

    assert_success(&output);
    // The producer: its own variable and its own hook's, nothing of `app`.
    assert_eq!(
        repository.read("content/out/env.txt")?,
        "content-own|from-content-hook|unset|unset"
    );
    // The consumer: its own secret and its own hook's, nothing of `content`.
    assert_eq!(
        repository.read("app/env.txt")?,
        "app-only|from-app-hook|unset|unset"
    );
    Ok(())
}

#[test]
fn cuenv_task_gives_a_dependency_its_own_environment() -> TestResult {
    let repository = Monorepo::changing(
        APP_WITH_HOOK,
        &[("content/env.cue", CONTENT_WITH_HOOK)],
        AN_ARTICLE_CHANGES,
    )?;

    let output = repository.task("app", &["build"])?;

    assert_success(&output);
    // `cuenv task` runs no hooks. Each project still sees only its own
    // variables.
    assert_eq!(
        repository.read("content/out/env.txt")?,
        "content-own|unset|unset|unset"
    );
    assert_eq!(
        repository.read("app/env.txt")?,
        "app-only|unset|unset|unset"
    );
    Ok(())
}

#[test]
fn a_dependency_chain_across_three_projects_runs_in_order() -> TestResult {
    // `app` consumes `content`, which consumes `sources`. Neither of the
    // other two is under `--path app`.
    let repository = Monorepo::changing(
        APP_CONSUMING_CONTENT,
        &[
            (
                "content/env.cue",
                r#"package cuenv
name: "content"
tasks: articles: {
	command: "sh"
	args: ["-c", "mkdir -p out && cat articles/one.md in/raw.md > out/one.md"]
	inputs: ["articles/**", {project: "sources", task: "pull", map: [{from: "raw.md", to: "in/raw.md"}]}]
	outputs: ["out/one.md"]
}
"#,
            ),
            (
                "sources/env.cue",
                r#"package cuenv
name: "sources"
tasks: pull: {
	command: "sh"
	args: ["-c", "printf 'raw\n' > raw.md"]
	inputs: ["env.cue"]
	outputs: ["raw.md"]
}
"#,
            ),
        ],
        AN_ARTICLE_CHANGES,
    )?;

    let output = repository.ci(&["--path", "app"])?;

    assert_success(&output);
    assert_eq!(repository.read("sources/raw.md")?, "raw\n");
    assert_eq!(repository.read("content/out/one.md")?, "two\nraw\n");
    assert_eq!(repository.read("app/seen.md")?, "two\nraw\n");

    // `cuenv task` builds the same graph.
    let task = repository.task("app", &["build"])?;
    assert_success(&task);
    assert_eq!(repository.read("app/seen.md")?, "two\nraw\n");
    Ok(())
}

/// `app` consuming `content`, with the project spelled as given.
fn app_consuming(project: &str) -> String {
    APP_CONSUMING_CONTENT.replace("project: \"content\"", &format!("project: \"{project}\""))
}

#[test]
fn a_project_spelled_as_a_path_is_selected_and_run_like_one_spelled_by_name() -> TestResult {
    // By name, from the project that depends on it, and from the root of
    // the checkout. Selection and execution find the same project by each.
    for spelling in ["content", "../content", "/content"] {
        let app = app_consuming(spelling);

        // A file of `content` changed: `app` is selected, and `content`
        // runs first.
        let repository = Monorepo::new(&app)?;
        let output = repository.ci(&["--path", "app"])?;
        assert_success(&output);
        let printed = text(&output);
        assert!(!printed.contains("warning:"), "{spelling}: {printed}");
        assert_eq!(
            repository.read("content/out/one.md")?,
            "two\n",
            "{spelling}"
        );
        assert_eq!(repository.read("app/seen.md")?, "two\n", "{spelling}");

        // Nothing either project reads changed. The dependency was found,
        // so it is shown to be unaffected and nothing runs.
        let unaffected = Monorepo::changing(&app, &[], &[("docs/readme.md", "changed\n")])?;
        let output = unaffected.ci(&["--path", "app"])?;
        assert_success(&output);
        let printed = text(&output);
        assert!(
            printed.contains("No affected tasks"),
            "{spelling}: {printed}"
        );
        assert!(!printed.contains("warning:"), "{spelling}: {printed}");
        assert!(
            !unaffected.root().join("app/seen.md").exists(),
            "{spelling}: {printed}"
        );
    }
    Ok(())
}

#[test]
fn a_dependency_in_a_module_nested_in_the_repository_is_staged_from_the_module() -> TestResult {
    // The module is `<repository>/mono`; the checkout root is its parent.
    // A mapped input names files from the module root, and the repository
    // holds a file where the same path leads from its own root.
    for spelling in ["content", "../content", "/mono/content"] {
        let directory = tempfile::tempdir()?;
        let root = directory.path();
        let module = root.join("mono");
        write(&module, "cue.mod/module.cue", MODULE)?;
        write(&module, "app/env.cue", &app_consuming(spelling))?;
        write(&module, "app/src/main.ts", "export {}\n")?;
        write(&module, "content/env.cue", CONTENT)?;
        write(&module, "content/articles/one.md", "one\n")?;
        write(root, "content/out/one.md", "the wrong file\n")?;
        test_git::init_repository(root)?;
        let base = test_git::commit_all(root, "base")?;
        write(&module, "content/articles/one.md", "two\n")?;
        test_git::commit_all(root, "an article changes")?;

        let output = Command::new(env!("CARGO_BIN_EXE_cuenv"))
            .current_dir(&module)
            .env_remove("GITHUB_ACTIONS")
            .env_remove("BUILDKITE")
            .env("CUENV_STATE_DIR", root.join("state"))
            .args(["ci", "--from", &base, "--path", "app"])
            .output()?;

        assert_success(&output);
        let printed = text(&output);
        assert!(!printed.contains("warning:"), "{spelling}: {printed}");
        assert_eq!(
            fs::read_to_string(module.join("content/out/one.md"))?,
            "two\n",
            "{spelling}"
        );
        assert_eq!(
            fs::read_to_string(module.join("app/seen.md"))?,
            "two\n",
            "{spelling}"
        );
    }
    Ok(())
}

#[test]
fn a_dependency_that_cannot_be_loaded_fails_the_run() -> TestResult {
    // The dependency is neither skipped nor run in the consumer's project.
    let repository = Monorepo::new(&app_consuming("no-such-project"))?;

    let output = repository.ci(&["--path", "app"])?;

    let printed = text(&output);
    assert!(!output.status.success(), "{printed}");
    assert!(printed.contains("#no-such-project:articles"), "{printed}");
    assert!(!repository.root().join("app/seen.md").exists(), "{printed}");
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
        &repository.root(),
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

    // A real run executes the task of `content` itself, in `content`.
    let output = repository.ci(&["--path", "app"])?;
    assert_success(&output);
    assert_eq!(repository.read("content/out/one.md")?, "two\n");
    assert!(!repository.root().join("content/drafts/out").exists());
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

/// An `app` whose `build` task writes `ran` and declares these inputs.
fn app_reading(inputs: &str) -> String {
    format!(
        r#"package cuenv
name: "app"
let _tasks = tasks
ci: pipelines: default: tasks: [_tasks.build]
tasks: build: {{
	command: "sh"
	args: ["-c", "printf ran > ran"]
	inputs: {inputs}
	hermetic: sandbox: "dir"
}}
"#
    )
}

#[test]
fn an_input_is_matched_however_it_is_spelled() -> TestResult {
    // Each of these was hashed as an input and never matched a change.
    for (inputs, changed) in [
        (r#"["./lock.txt"]"#, "app/lock.txt"),
        (r#"["src/{a,b}.txt"]"#, "app/src/a.txt"),
        (r#"["src/** "]"#, "app/src/a.txt"),
        (r#"["src/./"]"#, "app/src/a.txt"),
        (r#"["."]"#, "app/anything.txt"),
    ] {
        let repository = Monorepo::changing(
            &app_reading(inputs),
            &[("app/lock.txt", "1"), ("app/src/a.txt", "1")],
            &[(changed, "2")],
        )?;

        let output = repository.ci(&["--path", "app"])?;

        assert_success(&output);
        assert!(
            repository.root().join("app/ran").exists(),
            "inputs {inputs}, changed {changed}: {}",
            text(&output)
        );
    }
    Ok(())
}

#[test]
fn a_checkout_named_like_its_project_still_selects_the_project() -> TestResult {
    // The checkout is `<tmp>/app` and the project is `app`.
    let repository = Monorepo::laid_out(&Layout {
        checkout: Path::new("app"),
        app: &app_reading(r#"["src/**"]"#),
        files: &[],
        changes: &[("app/src/main.ts", "export const changed = true\n")],
    })?;

    let output = repository.ci(&["--path", "app"])?;

    assert_success(&output);
    assert!(
        repository.root().join("app/ran").exists(),
        "{}",
        text(&output)
    );
    Ok(())
}

#[test]
fn an_input_that_only_matches_from_the_repository_root_still_runs_and_says_so() -> TestResult {
    // `app` reads `shared/**`, and the only `shared/` is at the root.
    let repository = Monorepo::changing(
        &app_reading(r#"["src/**", "shared/**"]"#),
        &[("shared/x.txt", "1")],
        &[("shared/x.txt", "2")],
    )?;

    let output = repository.ci(&["--path", "app"])?;

    assert_success(&output);
    assert!(repository.root().join("app/ran").exists());
    let printed = text(&output);
    assert!(
        printed.contains("has input 'shared/**', which selects nothing under app"),
        "{printed}"
    );
    Ok(())
}

#[test]
fn a_correct_project_is_not_warned_about_files_of_the_same_name_at_the_root() -> TestResult {
    let repository = Monorepo::changing(
        &app_reading(r#"["package.json", "src/**"]"#),
        &[("app/package.json", "{}"), ("package.json", "{}")],
        &[("package.json", "{ }")],
    )?;

    let output = repository.ci(&["--path", "app"])?;

    // The root file still selects the task for now, and nothing is said:
    // the configuration is correct.
    assert_success(&output);
    assert!(repository.root().join("app/ran").exists());
    assert!(!text(&output).contains("warning:"), "{}", text(&output));
    Ok(())
}

/// An `app` that imports a shared package for the name of the file its task
/// writes, with a base in the module root that `app` inherits.
const APP_BUILT_FROM_SHARED_CUE: &str = r#"package cuenv

import "test.example/monorepo/shared"

name: "app"
let _tasks = tasks
ci: pipelines: default: tasks: [_tasks.build]
tasks: build: {
	command: "sh"
	args: ["-c", "printf ran > \(shared.marker)"]
	inputs: ["src/**"]
	hermetic: sandbox: "dir"
}
"#;

fn monorepo_built_from_shared_cue(changes: &[(&str, &str)]) -> TestResult<Monorepo> {
    Monorepo::changing(
        APP_BUILT_FROM_SHARED_CUE,
        &[
            ("env.cue", "package cuenv\n\nenv: SHARED: \"base\"\n"),
            ("shared/shared.cue", "package shared\n\nmarker: \"ran\"\n"),
            ("unrelated/notes.cue", "package notes\n\ntext: \"one\"\n"),
        ],
        changes,
    )
}

#[test]
fn a_change_to_the_cue_a_project_is_built_from_runs_its_tasks() -> TestResult {
    // None of these files is a declared input of `build`.
    for (changed, content) in [
        // The project's own file.
        (
            "app/env.cue",
            format!("{APP_BUILT_FROM_SHARED_CUE}\n// a comment\n"),
        ),
        // A file of a directory above it, in the same package.
        (
            "env.cue",
            "package cuenv\n\nenv: SHARED: \"changed\"\n".to_string(),
        ),
        // A package it imports.
        (
            "shared/shared.cue",
            "package shared\n\nmarker: \"ran\"\n// changed\n".to_string(),
        ),
        // A new file in a directory that contributes to it.
        ("app/extra.cue", "package cuenv\n".to_string()),
        // The module's own metadata.
        ("cue.mod/module.cue", format!("{MODULE}// a comment\n")),
    ] {
        let repository = monorepo_built_from_shared_cue(&[(changed, &content)])?;

        let output = repository.ci(&["--path", "app"])?;

        assert_success(&output);
        assert!(
            repository.root().join("app/ran").exists(),
            "{changed} changed: {}",
            text(&output)
        );
    }
    Ok(())
}

#[test]
fn a_change_to_cue_a_project_is_not_built_from_does_not_run_its_tasks() -> TestResult {
    for (changed, content) in [
        ("unrelated/notes.cue", "package notes\n\ntext: \"two\"\n"),
        (
            "docs/env.cue",
            "package cuenv\nname: \"docs\"\ntasks: lint: {command: \"true\"}\n",
        ),
        // A `.cue` file beneath the project that is neither part of its
        // instance nor one of its inputs.
        ("app/vendor/data.cue", "package data\n"),
    ] {
        let repository = monorepo_built_from_shared_cue(&[(changed, content)])?;

        let output = repository.ci(&["--path", "app"])?;

        assert_success(&output);
        assert!(
            !repository.root().join("app/ran").exists(),
            "{changed} changed: {}",
            text(&output)
        );
    }
    Ok(())
}

#[test]
fn a_deleted_cue_file_of_a_project_runs_its_tasks() -> TestResult {
    let directory = tempfile::tempdir()?;
    let root = directory.path();
    write(root, "cue.mod/module.cue", MODULE)?;
    write(root, "app/env.cue", &app_reading(r#"["src/**"]"#))?;
    write(root, "app/extra.cue", "package cuenv\n")?;
    write(root, "app/src/main.ts", "export {}\n")?;
    test_git::init_repository(root)?;
    let base = test_git::commit_all(root, "base")?;
    fs::remove_file(root.join("app/extra.cue"))?;
    test_git::commit_all(root, "a CUE file is deleted")?;
    let repository = Monorepo {
        directory,
        checkout: std::path::PathBuf::new(),
        base,
    };

    let output = repository.ci(&["--path", "app"])?;

    assert_success(&output);
    assert!(
        repository.root().join("app/ran").exists(),
        "{}",
        text(&output)
    );
    Ok(())
}

#[test]
fn deleting_the_last_cue_file_of_a_directory_above_a_project_runs_its_tasks() -> TestResult {
    // `services/defaults.cue` is the only file of the package in
    // `services/`, and `services/app` inherits it. Once it is deleted, the
    // loader no longer reports `services/` as part of the instance.
    let directory = tempfile::tempdir()?;
    let root = directory.path();
    write(root, "cue.mod/module.cue", MODULE)?;
    write(root, "services/defaults.cue", "package cuenv\n")?;
    write(root, "services/app/env.cue", &app_reading(r#"["src/**"]"#))?;
    write(root, "services/app/src/main.ts", "export {}\n")?;
    test_git::init_repository(root)?;
    let base = test_git::commit_all(root, "base")?;
    fs::remove_file(root.join("services/defaults.cue"))?;
    test_git::commit_all(root, "the inherited file is deleted")?;
    let repository = Monorepo {
        directory,
        checkout: std::path::PathBuf::new(),
        base,
    };

    let output = repository.ci(&["--path", "services/app"])?;

    assert_success(&output);
    assert!(
        repository.root().join("services/app/ran").exists(),
        "{}",
        text(&output)
    );
    Ok(())
}

#[test]
fn a_cue_change_in_a_module_nested_in_the_repository_runs_the_tasks() -> TestResult {
    // The module is `<repository>/mono`; the checkout root is its parent.
    let directory = tempfile::tempdir()?;
    let root = directory.path();
    let module = root.join("mono");
    write(&module, "cue.mod/module.cue", MODULE)?;
    write(&module, "app/env.cue", &app_reading(r#"["src/**"]"#))?;
    write(&module, "app/src/main.ts", "export {}\n")?;
    // A namesake outside the module must not count.
    write(root, "app/env.cue", "package other\n")?;
    test_git::init_repository(root)?;
    let base = test_git::commit_all(root, "base")?;

    let run = |repository: &Path| -> TestResult<Output> {
        Ok(Command::new(env!("CARGO_BIN_EXE_cuenv"))
            .current_dir(repository.join("mono"))
            .env_remove("GITHUB_ACTIONS")
            .env_remove("BUILDKITE")
            .env("CUENV_STATE_DIR", repository.join("state"))
            .args(["ci", "--from", &base, "--path", "app"])
            .output()?)
    };

    write(root, "app/env.cue", "package other\n// changed\n")?;
    test_git::commit_all(root, "a file outside the module changes")?;
    let output = run(root)?;
    assert_success(&output);
    assert!(!module.join("app/ran").exists(), "{}", text(&output));

    write(
        &module,
        "app/env.cue",
        &format!("{}// changed\n", app_reading(r#"["src/**"]"#)),
    )?;
    test_git::commit_all(root, "the project's CUE changes")?;
    let output = run(root)?;
    assert_success(&output);
    assert!(module.join("app/ran").exists(), "{}", text(&output));
    Ok(())
}
