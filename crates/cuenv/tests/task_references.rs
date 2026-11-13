//! End-to-end contracts for task references through imports (issue 611): a
//! task depends on a task of another project as `dependsOn:
//! [content.tasks.build]` and reads the files it declares as `inputs:
//! [content.tasks.build.outputFiles]`, both through `content`'s import.
//!
//! The other project's task runs first, in its own project. A same-named
//! task of the consumer is never bound in its place. What cannot be named
//! across projects (a whole sequence, a step of one) and what is no input
//! (a task, a group, a path that leaves the project) is an error that says
//! what to write instead.

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

/// `content`: a task that builds a file, a group, a sequence, and a task
/// that declares no outputs.
const CONTENT: &str = r#"package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project
name: "content"
tasks: {
	build: {
		command: "sh"
		args: ["-c", "mkdir -p dist && cat articles/one.md > dist/built.txt"]
		inputs: ["articles/**"]
		outputs: ["dist/built.txt"]
	}
	checks: {
		type: "group"
		lint: {
			command: "sh"
			args: ["-c", "printf linted > linted.txt"]
			inputs: ["articles/**"]
			outputs: ["linted.txt"]
		}
	}
	steps: [{command: "true", inputs: ["articles/**"]}]
	plain: {command: "true", inputs: ["articles/**"]}
}
"#;

/// `app`: a `build` of its own, and `site`, which depends on `content`'s
/// `build` through a `let` and reads the file it builds.
const APP: &str = r#"package cuenv

import (
	"github.com/cuenv/cuenv/schema"
	content "github.com/cuenv/cuenv/content:cuenv"
)

schema.#Project
name: "app"
let _tasks = tasks
let _content = content.tasks
ci: providers: ["github"]
ci: pipelines: default: {tasks: [_tasks.site], when: branch: "main"}
tasks: {
	build: {
		command: "sh"
		args: ["-c", "printf local > local.txt"]
		inputs: ["src/**"]
		outputs: ["local.txt"]
	}
	site: {
		command: "sh"
		args: ["-c", "cat ../content/dist/built.txt > site.txt"]
		dependsOn: [_content.build]
		inputs: ["src/**", content.tasks.build.outputFiles]
		outputs: ["site.txt"]
	}
	lint: {
		command: "true"
		dependsOn: [content.tasks.checks]
		inputs: ["src/**"]
	}
	steps: {
		command: "true"
		dependsOn: [content.tasks.steps]
		inputs: ["src/**"]
	}
	plain: {
		command: "true"
		inputs: [content.tasks.plain.outputFiles]
	}
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
        // CUE ignores directories whose name starts with a dot.
        let directory = tempfile::Builder::new().prefix("cuenv_test_").tempdir()?;
        let root = directory.path();
        write(root, "cue.mod/module.cue", MODULE)?;
        copy_cue_files(
            &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schema"),
            &root.join("schema"),
        )?;
        write(root, "content/env.cue", CONTENT)?;
        write(root, "content/articles/one.md", "one\n")?;
        write(root, "app/env.cue", app)?;
        write(root, "app/src/main.ts", "export {}\n")?;
        write(root, "docs/readme.md", "docs\n")?;
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

    fn exists(&self, relative: &str) -> bool {
        self.root().join(relative).exists()
    }
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

/// The order a dry run would run tasks in, as `cuenv task --dry-run` prints
/// it.
fn execution_order(output: &Output) -> TestResult<Vec<String>> {
    let plan: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    Ok(plan["execution_order"]
        .as_array()
        .ok_or("a dry run lists its execution order")?
        .iter()
        .filter_map(serde_json::Value::as_str)
        .map(str::to_string)
        .collect())
}

#[test]
fn a_task_of_another_project_runs_first_and_its_files_are_read() -> TestResult {
    let repository = Repository::new(APP, &[])?;

    let output = repository.task("app", &["site"])?;

    assert_success(&output);
    // `content`'s build ran in `content`; the local `build` did not run.
    assert_eq!(repository.read("content/dist/built.txt")?, "one\n");
    assert_eq!(repository.read("app/site.txt")?, "one\n");
    assert!(!repository.exists("app/local.txt"));
    Ok(())
}

#[test]
fn a_task_of_another_project_is_named_by_its_project() -> TestResult {
    let repository = Repository::new(APP, &[])?;

    let output = repository.task("app", &["site", "--dry-run"])?;

    assert_success(&output);
    let order = execution_order(&output)?;
    assert_eq!(order, ["#content:build", "site"], "{}", text(&output));
    Ok(())
}

#[test]
fn a_group_of_another_project_runs_as_a_dependency() -> TestResult {
    let repository = Repository::new(APP, &[])?;

    let output = repository.task("app", &["lint"])?;

    assert_success(&output);
    assert_eq!(repository.read("content/linted.txt")?, "linted");
    Ok(())
}

#[test]
fn a_sequence_of_another_project_cannot_be_depended_on() -> TestResult {
    let repository = Repository::new(APP, &[])?;

    let output = repository.task("app", &["steps"])?;

    assert_failure(&output);
    let printed = text(&output);
    assert!(
        printed.contains("depend on a named task or group of project 'content'"),
        "{printed}"
    );
    // A task that does not reach it is unaffected.
    assert_success(&repository.task("app", &["site"])?);
    Ok(())
}

#[test]
fn reading_the_outputs_of_a_task_that_declares_none_is_an_error() -> TestResult {
    let repository = Repository::new(APP, &[])?;

    let output = repository.task("app", &["plain"])?;

    assert_failure(&output);
    let printed = text(&output);
    assert!(
        printed
            .contains("reads the `outputFiles` of '#content:plain', which declares no `outputs`"),
        "{printed}"
    );
    Ok(())
}

#[test]
fn an_input_that_leaves_the_project_is_rejected_with_its_rewrite() -> TestResult {
    let app = APP.replace(
        "inputs: [\"src/**\", content.tasks.build.outputFiles]",
        "inputs: [\"src/**\", \"../content/articles/**\"]",
    );
    let repository = Repository::new(&app, &[])?;

    let output = repository.task("app", &["build"])?;

    assert_failure(&output);
    let printed = text(&output);
    assert!(
        printed.contains("an input cannot leave the project directory"),
        "{printed}"
    );
    assert!(
        printed.contains("`../content/articles/**` is `content/articles/**` from the module root"),
        "{printed}"
    );
    Ok(())
}

#[test]
fn a_task_or_a_group_used_as_an_input_says_what_to_write_instead() -> TestResult {
    for (input, hint) in [
        (
            "content.tasks.build",
            "a task is not an input: write `<task>.outputFiles`",
        ),
        ("content.tasks.checks", "groups cannot be inputs yet"),
    ] {
        let app = APP.replace(
            "inputs: [\"src/**\", content.tasks.build.outputFiles]",
            &format!("inputs: [\"src/**\", {input}]"),
        );
        let repository = Repository::new(&app, &[])?;

        let output = repository.task("app", &["build"])?;

        assert_failure(&output);
        let printed = text(&output);
        assert!(printed.contains(hint), "{input}: {printed}");
    }
    Ok(())
}

/// `web`, which imports a package of another module, vendored under
/// `cue.mod/pkg`, whose project is named `web` too.
const WEB: &str = r#"package cuenv

import (
	"github.com/cuenv/cuenv/schema"
	other "example.com/other/web:cuenv"
)

schema.#Project
name: "web"
tasks: {
	build: {command: "sh", args: ["-c", "printf local > local.txt"], outputs: ["local.txt"]}
	site: {command: "true", dependsOn: [other.tasks.build]}
	own: {command: "true", dependsOn: [build]}
}
"#;

/// The vendored namesake of `web`.
const NAMESAKE: &str = r#"package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project
name: "web"
tasks: build: {command: "sh", args: ["-c", "printf vendored > vendored.txt"]}
"#;

#[test]
fn a_namesake_project_of_another_module_is_never_this_project() -> TestResult {
    let repository = Repository::new(
        APP,
        &[
            ("web/env.cue", WEB),
            ("cue.mod/pkg/example.com/other/web/env.cue", NAMESAKE),
        ],
    )?;

    let output = repository.task("web", &["site"])?;

    // The vendored `build` is another project's, which references cannot
    // reach; the local `build` never runs in its place.
    assert_failure(&output);
    let printed = text(&output);
    assert!(
        printed.contains("references reach projects of the same CUE module only"),
        "{printed}"
    );
    assert!(!repository.exists("web/local.txt"));
    // A dependency on the local `build` is still the local one.
    assert_success(&repository.task("web", &["own"])?);
    assert_eq!(repository.read("web/local.txt")?, "local");
    Ok(())
}

#[test]
fn a_change_to_the_producers_inputs_selects_the_consumer_in_ci() -> TestResult {
    let repository = Repository::new(APP, &[("content/articles/one.md", "two\n")])?;

    let output = repository.ci(&["--path", "app"])?;

    assert_success(&output);
    let printed = text(&output);
    assert!(!printed.contains("No affected tasks"), "{printed}");
    assert!(!printed.contains("warning:"), "{printed}");
    assert_eq!(repository.read("content/dist/built.txt")?, "two\n");
    assert_eq!(repository.read("app/site.txt")?, "two\n");
    assert!(!repository.exists("app/local.txt"));
    Ok(())
}

#[test]
fn a_change_nothing_reads_selects_nothing_in_ci() -> TestResult {
    let repository = Repository::new(APP, &[("docs/readme.md", "changed\n")])?;

    for arguments in [vec!["--path", "app", "--dry-run"], vec!["--dry-run"]] {
        let output = repository.ci(&arguments)?;

        assert_success(&output);
        let printed = text(&output);
        assert!(printed.contains("No affected tasks"), "{printed}");
    }
    assert!(!repository.exists("content/dist"));
    Ok(())
}

#[test]
fn sync_ci_triggers_a_thin_workflow_on_the_producers_inputs() -> TestResult {
    let repository = Repository::new(APP, &[])?;

    let output = repository.sync_ci()?;

    assert_success(&output);
    let workflow = repository.read(".github/workflows/app-default.yml")?;
    for path in ["app/src/**", "content/articles/**", "content/*.cue"] {
        assert!(workflow.contains(path), "{path}: {workflow}");
    }
    Ok(())
}

/// `sources`: a project `content` imports and `app` does not.
const SOURCES: &str = r#"package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project
name: "sources"
tasks: fetch: {command: "true", inputs: ["feeds/**"], outputs: ["feed.txt"]}
"#;

/// `content`, whose `build` depends on a task of its own, `gen`, which
/// depends on a task of `sources`.
fn content_with_dependencies() -> String {
    CONTENT
        .replace(
            "import \"github.com/cuenv/cuenv/schema\"",
            "import (\n\t\"github.com/cuenv/cuenv/schema\"\n\tsources \
             \"github.com/cuenv/cuenv/sources:cuenv\"\n)",
        )
        .replace(
            "tasks: {\n\tbuild: {\n",
            "tasks: {\n\tgen: {command: \"true\", dependsOn: [sources.tasks.fetch], inputs: \
             [\"drafts/**\"], outputs: [\"gen.txt\"]}\n\tbuild: {\n\t\tdependsOn: [gen]\n",
        )
}

#[test]
fn sync_ci_follows_the_producers_own_dependencies_and_what_it_imports() -> TestResult {
    let content = content_with_dependencies();
    // Through a `let`, a dependency two projects deep evaluates for over a
    // minute in the bridge: name the task through the import.
    let app = APP
        .replace("let _content = content.tasks\n", "")
        .replace("[_content.build]", "[content.tasks.build]");
    let repository = Repository::new(
        &app,
        &[
            ("content/env.cue", content.as_str()),
            ("sources/env.cue", SOURCES),
        ],
    )?;

    let output = repository.sync_ci()?;

    assert_success(&output);
    let workflow = repository.read(".github/workflows/app-default.yml")?;
    for path in [
        "content/articles/**",
        "content/drafts/**",
        "sources/feeds/**",
    ] {
        assert!(workflow.contains(path), "{path}: {workflow}");
    }
    Ok(())
}

#[test]
fn a_workflow_of_a_job_per_task_cannot_run_a_task_of_another_project() -> TestResult {
    let app = APP.replace(
        "when: branch: \"main\"}",
        "when: branch: \"main\", mode: \"expanded\"}",
    );
    let repository = Repository::new(&app, &[])?;

    let output = repository.sync_ci()?;

    assert_failure(&output);
    let printed = text(&output);
    assert!(
        printed.contains("task 'site' depends on '#content:build', a task of another project"),
        "{printed}"
    );
    assert!(printed.contains("mode: \"thin\""), "{printed}");
    Ok(())
}

#[test]
fn a_workflow_of_a_job_per_task_cannot_run_a_groups_dependency_on_another_project() -> TestResult {
    let app = APP
        .replace(
            "tasks: [_tasks.site], when: branch: \"main\"}",
            "tasks: [_tasks.checks], when: branch: \"main\", mode: \"expanded\"}",
        )
        .replace(
            "tasks: {\n",
            "tasks: {\n\tchecks: {\n\t\ttype: \"group\"\n\t\tdependsOn: [content.tasks.build, \
             build]\n\t\tunit: {command: \"true\", inputs: [\"src/**\"]}\n\t}\n",
        );
    let repository = Repository::new(&app, &[])?;

    let output = repository.sync_ci()?;

    assert_failure(&output);
    let printed = text(&output);
    assert!(
        printed
            .contains("task 'checks.unit' depends on '#content:build', a task of another project"),
        "{printed}"
    );
    Ok(())
}

#[test]
fn a_job_of_a_group_member_waits_for_the_groups_dependencies() -> TestResult {
    let app = APP
        .replace(
            "tasks: [_tasks.site], when: branch: \"main\"}",
            "tasks: [_tasks.checks], when: branch: \"main\", mode: \"expanded\"}",
        )
        .replace(
            "tasks: {\n",
            "tasks: {\n\tchecks: {\n\t\ttype: \"group\"\n\t\tdependsOn: [build]\n\t\tunit: \
             {command: \"true\", inputs: [\"src/**\"]}\n\t}\n",
        );
    let repository = Repository::new(&app, &[])?;

    let output = repository.sync_ci()?;

    assert_success(&output);
    let workflow: serde_yaml::Value =
        serde_yaml::from_str(&repository.read(".github/workflows/app-default.yml")?)?;
    let jobs = &workflow["jobs"];
    assert!(!jobs["build"].is_null(), "a job runs `build`: {jobs:?}");
    let needs = serde_yaml::to_string(&jobs["checks-unit"]["needs"])?;
    assert!(needs.contains("build"), "{needs}");
    Ok(())
}

/// A chain of `depth` projects whose two tasks each depend on both tasks of
/// the next project through its import, and read one of its outputs.
fn chain_of_projects(depth: usize) -> TestResult<TempDir> {
    let directory = tempfile::Builder::new().prefix("cuenv_chain_").tempdir()?;
    let root = directory.path();
    write(root, "cue.mod/module.cue", MODULE)?;
    copy_cue_files(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schema"),
        &root.join("schema"),
    )?;
    for index in 0..depth {
        let next = index + 1;
        let project = if next < depth {
            format!(
                "package cuenv\n\nimport (\n\t\"github.com/cuenv/cuenv/schema\"\n\tnext \
                 \"github.com/cuenv/cuenv/p{next}:cuenv\"\n)\n\nschema.#Project\nname: \
                 \"p{index}\"\ntasks: {{\n\ta: {{command: \"true\", dependsOn: [next.tasks.a, \
                 next.tasks.b], inputs: [next.tasks.a.outputFiles], outputs: [\"a.txt\"]}}\n\tb: \
                 {{command: \"true\", dependsOn: [next.tasks.a, next.tasks.b], outputs: \
                 [\"b.txt\"]}}\n}}\n"
            )
        } else {
            format!(
                "package cuenv\n\nimport \"github.com/cuenv/cuenv/schema\"\n\nschema.#Project\n\
                 name: \"p{index}\"\ntasks: {{\n\ta: {{command: \"true\", outputs: \
                 [\"a.txt\"]}}\n\tb: {{command: \"true\", outputs: [\"b.txt\"]}}\n}}\n"
            )
        };
        write(root, &format!("p{index}/env.cue"), &project)?;
    }
    Ok(directory)
}

#[test]
fn a_deep_chain_of_projects_evaluates_without_exploding() -> TestResult {
    // Stock CUE takes over a minute to export the first project of this
    // chain; the bridge shares typed references into imported packages and
    // evaluates it in about a second. The bound only catches an exponential
    // regression, never a slow machine. The chain is generated here, never
    // checked in: plain `cue export` of it is what explodes.
    let chain = chain_of_projects(8)?;
    let started = std::time::Instant::now();

    let output = Command::new(env!("CARGO_BIN_EXE_cuenv"))
        .current_dir(chain.path().join("p0"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", std::env::var("HOME").unwrap_or_default())
        .env("CUENV_STATE_DIR", chain.path().join("state"))
        .args(["task", "a", "--dry-run"])
        .output()?;

    assert_success(&output);
    let elapsed = started.elapsed();
    assert!(
        elapsed < std::time::Duration::from_secs(60),
        "took {elapsed:?}"
    );
    let order = execution_order(&output)?;
    assert_eq!(order.len(), 15, "{order:?}");
    assert_eq!(order.last().map(String::as_str), Some("a"));
    Ok(())
}

#[test]
fn an_export_runs_the_task_of_another_project_in_the_consumers_step() -> TestResult {
    let repository = Repository::new(APP, &[])?;

    let output = repository.ci(&["--export", "buildkite"])?;

    assert_success(&output);
    let pipeline = String::from_utf8_lossy(&output.stdout);
    // One step runs `cuenv ci`, which runs the dependency; no step depends
    // on a step named after it.
    assert!(
        pipeline.contains("cuenv ci --pipeline default"),
        "{pipeline}"
    );
    assert!(!pipeline.contains("#content:build"), "{pipeline}");
    Ok(())
}
