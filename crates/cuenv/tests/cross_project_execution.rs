//! End-to-end contracts for running a task that depends on a task of another
//! project (issue 611).
//!
//! The other project's task runs first, in its own directory and with its
//! own environment. It is part of the graph only: it cannot be asked for by
//! name or by label. What the consuming task reads is staged in one frame,
//! whether or not the CUE module is the root of its repository.

#[path = "../src/test_git.rs"]
mod test_git;

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

const MODULE: &str = "module: \"test.example/monorepo\"\nlanguage: version: \"v0.14.0\"\n";

/// `content`: renders an article into `out/`, and records the environment it
/// ran with beside it.
const CONTENT: &str = r#"package cuenv
name: "content"
env: {
	CONTENT_VALUE: "content-own"
	environment: production: CONTENT_VALUE: "content-production"
}
tasks: articles: {
	command: "sh"
	args: ["-c", "mkdir -p out && cp articles/one.md out/one.md && printf '%s|%s' \"${CONTENT_VALUE:-unset}\" \"${APP_SECRET:-unset}\" > out/env.txt"]
	inputs: ["articles/**"]
	outputs: ["out/one.md", "out/env.txt"]
	labels: ["publish"]
}
"#;

/// `app`: reads the article `content` rendered, at the path it mapped it to.
/// `APP_SECRET` is a secret of `app`, and of no other project.
const APP: &str = r#"package cuenv
name: "app"
env: {
	APP_SECRET: {resolver: "exec", command: "echo", args: ["app-only"]}
	APP_MODE: "development"
	environment: production: APP_MODE: "production"
}
tasks: {
	build: {
		command: "sh"
		args: ["-c", "cp vendor/one.md seen.md && printf '%s|%s|%s' \"${APP_SECRET:-unset}\" \"${APP_MODE:-unset}\" \"${CONTENT_VALUE:-unset}\" > env.txt"]
		inputs: ["src/**", {project: "content", task: "articles", map: [{from: "out/one.md", to: "vendor/one.md"}]}]
		outputs: ["seen.md", "env.txt"]
	}
	lint: {
		command: "sh"
		args: ["-c", "printf linted > linted.txt"]
		inputs: ["src/**"]
		outputs: ["linted.txt"]
	}
}
"#;

/// `sources`: records the one variable it has. It defines no environments.
const SOURCES: &str = r#"package cuenv
name: "sources"
env: SOURCES_VALUE: "sources-base"
tasks: pull: {
	command: "sh"
	args: ["-c", "printf '%s' \"${SOURCES_VALUE:-unset}\" > env.txt"]
	inputs: ["env.cue"]
	outputs: ["env.txt"]
}
"#;

/// A CUE module of projects in a temporary directory, with a task cache of
/// its own so that no run is answered from another's results.
struct Workspace {
    directory: TempDir,
    /// The module's directory below the temporary one; empty when the
    /// module is the root.
    module: PathBuf,
}

impl Workspace {
    /// A module at the root of the directory.
    fn new() -> TestResult<Self> {
        Self::below("")
    }

    /// A module in `module`, below the root of the directory.
    fn below(module: &str) -> TestResult<Self> {
        // CUE ignores directories whose name starts with a dot.
        let directory = tempfile::Builder::new().prefix("cuenv_test_").tempdir()?;
        let workspace = Self {
            directory,
            module: PathBuf::from(module),
        };
        workspace.write("cue.mod/module.cue", MODULE)?;
        Ok(workspace)
    }

    /// The two projects of the constants above.
    fn app_and_content() -> TestResult<Self> {
        let workspace = Self::new()?;
        workspace.write("app/env.cue", APP)?;
        workspace.write("app/src/main.ts", "export {}\n")?;
        workspace.write("content/env.cue", CONTENT)?;
        workspace.write("content/articles/one.md", "one\n")?;
        Ok(workspace)
    }

    fn root(&self) -> &Path {
        self.directory.path()
    }

    fn module_root(&self) -> PathBuf {
        self.root().join(&self.module)
    }

    /// Write a file of the module.
    ///
    /// A project's tasks run with a hermetic `PATH` that names the usual
    /// system directories, which hold no `cp` or `mkdir` in the Nix build
    /// sandbox. Each project the tests declare gets the test's own `PATH`.
    fn write(&self, relative: &str, content: &str) -> TestResult {
        let path = self.module_root().join(relative);
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

    /// Read a file of the module.
    fn read(&self, relative: &str) -> TestResult<String> {
        let path = self.module_root().join(relative);
        fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()).into())
    }

    fn exists(&self, relative: &str) -> bool {
        self.module_root().join(relative).exists()
    }

    /// Run `cuenv task` in the project `project` of the module.
    fn task(&self, project: &str, arguments: &[&str]) -> TestResult<Output> {
        Ok(Command::new(env!("CARGO_BIN_EXE_cuenv"))
            .current_dir(self.module_root().join(project))
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", std::env::var("HOME").unwrap_or_default())
            .env("USER", std::env::var("USER").unwrap_or_default())
            .env("CUENV_CACHE_DIR", self.root().join(".task-cache"))
            .arg("task")
            .args(arguments)
            .output()?)
    }
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

/// The names of a `--dry-run` graph, in the order they would run.
fn execution_order(output: &Output) -> TestResult<Vec<String>> {
    let graph: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    Ok(graph["execution_order"]
        .as_array()
        .ok_or("a dry run prints its execution order")?
        .iter()
        .filter_map(|name| name.as_str().map(str::to_string))
        .collect())
}

#[test]
fn a_task_of_another_project_runs_first_and_the_consumer_reads_its_output() -> TestResult {
    let workspace = Workspace::app_and_content()?;

    let output = workspace.task("app", &["build"])?;

    assert_success(&output);
    // The consumer read what the producer wrote, at the path it mapped it to.
    assert_eq!(workspace.read("app/seen.md")?, "one\n");
    // The producer ran in its own project and left its output there.
    assert_eq!(workspace.read("content/out/one.md")?, "one\n");
    // The mapping is staged in the consumer's execution root, not written
    // into the checkout.
    assert!(!workspace.exists("app/vendor"));
    assert_eq!(
        execution_order(&workspace.task("app", &["build", "--dry-run"])?)?,
        ["#content:articles", "build"]
    );
    Ok(())
}

#[test]
fn each_project_runs_with_its_own_environment_and_never_anothers_secrets() -> TestResult {
    let workspace = Workspace::app_and_content()?;

    let output = workspace.task("app", &["build"])?;

    assert_success(&output);
    // The producer saw its own variable, and not the consumer's secret.
    assert_eq!(workspace.read("content/out/env.txt")?, "content-own|unset");
    // The consumer saw its own, and not the producer's variable.
    assert_eq!(workspace.read("app/env.txt")?, "app-only|development|unset");
    Ok(())
}

#[test]
fn the_selected_environment_applies_to_every_project_that_defines_it() -> TestResult {
    let workspace = Workspace::app_and_content()?;
    // A third project that does not define `production` uses its base.
    workspace.write(
        "content/env.cue",
        &CONTENT.replace(
            "inputs: [\"articles/**\"]",
            "inputs: [\"articles/**\", {project: \"sources\", task: \"pull\", map: []}]",
        ),
    )?;
    workspace.write("sources/env.cue", SOURCES)?;

    let output = workspace.task("app", &["-e", "production", "build"])?;

    assert_success(&output);
    assert_eq!(workspace.read("app/env.txt")?, "app-only|production|unset");
    assert_eq!(
        workspace.read("content/out/env.txt")?,
        "content-production|unset"
    );
    assert_eq!(workspace.read("sources/env.txt")?, "sources-base");
    Ok(())
}

#[test]
fn a_task_of_another_project_cannot_be_selected_by_name_or_label() -> TestResult {
    let workspace = Workspace::app_and_content()?;

    let by_name = workspace.task("app", &["#content:articles"])?;
    assert_failure(&by_name);
    assert!(text(&by_name).contains("not found"), "{}", text(&by_name));

    let by_label = workspace.task("app", &["--label", "publish"])?;
    assert_failure(&by_label);
    assert!(
        text(&by_label).contains("No tasks with labels"),
        "{}",
        text(&by_label)
    );
    assert!(!workspace.exists("content/out"));

    // It is listed by its own project, where it can be asked for.
    let listed = workspace.task("app", &[])?;
    assert_success(&listed);
    assert!(!text(&listed).contains("articles"), "{}", text(&listed));
    Ok(())
}

#[test]
fn a_dependency_chain_across_three_projects_runs_in_order() -> TestResult {
    let workspace = Workspace::app_and_content()?;
    workspace.write(
        "content/env.cue",
        r#"package cuenv
name: "content"
tasks: articles: {
	command: "sh"
	args: ["-c", "mkdir -p out && cat articles/one.md in/raw.md > out/one.md"]
	inputs: ["articles/**", {project: "../sources", task: "pull", map: [{from: "raw.md", to: "in/raw.md"}]}]
	outputs: ["out/one.md"]
}
"#,
    )?;
    workspace.write(
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
    )?;

    let output = workspace.task("app", &["build"])?;

    assert_success(&output);
    assert_eq!(workspace.read("app/seen.md")?, "one\nraw\n");
    // `content` spells `sources` as a path and `app` spells `content` by
    // name: each project is one set of nodes however it is spelled, named
    // by its name once the module's names are known.
    assert_eq!(
        execution_order(&workspace.task("app", &["build", "--dry-run"])?)?,
        ["#sources:pull", "#content:articles", "build"]
    );
    Ok(())
}

/// A repository whose CUE module is the directory `services/`, with a
/// project that consumes task outputs in each of the three ways.
fn module_below_the_repository_root() -> TestResult<Workspace> {
    let workspace = Workspace::below("services")?;
    workspace.write(
        "app/env.cue",
        r#"package cuenv
name: "app"
tasks: {
	generate: {
		command: "sh"
		args: ["-c", "mkdir -p gen && printf generated > gen/data.txt"]
		inputs: ["src/**"]
		outputs: ["gen/data.txt"]
	}
	plain: {
		command: "sh"
		args: ["-c", "cp gen/data.txt plain.txt"]
		inputs: [{task: "generate"}]
		outputs: ["plain.txt"]
	}
	mapped: {
		command: "sh"
		args: ["-c", "cp vendor/data.txt mapped.txt"]
		inputs: [{task: "generate", map: [{from: "gen/data.txt", to: "vendor/data.txt"}]}]
		outputs: ["mapped.txt"]
	}
	cross: {
		command: "sh"
		args: ["-c", "cp vendor/one.md cross.txt"]
		inputs: [{project: "content", task: "articles", map: [{from: "out/one.md", to: "vendor/one.md"}]}]
		outputs: ["cross.txt"]
	}
}
"#,
    )?;
    workspace.write("app/src/main.ts", "export {}\n")?;
    workspace.write("content/env.cue", CONTENT)?;
    workspace.write("content/articles/one.md", "one\n")?;
    // A file the repository holds where the module-relative paths would
    // point if they were read from the repository root.
    fs::create_dir_all(workspace.root().join("app/gen"))?;
    fs::write(workspace.root().join("app/gen/data.txt"), "the wrong file")?;
    test_git::init_repository(workspace.root())?;
    test_git::commit_all(workspace.root(), "base")?;
    Ok(workspace)
}

#[test]
fn a_task_output_is_read_where_it_was_produced_when_the_module_is_not_the_repository_root()
-> TestResult {
    let workspace = module_below_the_repository_root()?;

    let output = workspace.task("app", &["plain"])?;

    assert_success(&output);
    assert_eq!(workspace.read("app/plain.txt")?, "generated");
    Ok(())
}

#[test]
fn a_mapped_task_output_is_staged_when_the_module_is_not_the_repository_root() -> TestResult {
    let workspace = module_below_the_repository_root()?;

    let output = workspace.task("app", &["mapped"])?;

    assert_success(&output);
    assert_eq!(workspace.read("app/mapped.txt")?, "generated");
    assert!(!workspace.exists("app/vendor"));
    Ok(())
}

#[test]
fn another_projects_output_is_staged_when_the_module_is_not_the_repository_root() -> TestResult {
    let workspace = module_below_the_repository_root()?;

    let output = workspace.task("app", &["cross"])?;

    assert_success(&output);
    assert_eq!(workspace.read("app/cross.txt")?, "one\n");
    assert_eq!(workspace.read("content/out/env.txt")?, "content-own|unset");
    Ok(())
}

#[test]
fn an_output_without_a_mapping_is_staged_where_its_producer_runs() -> TestResult {
    // `generate` runs in `tools/`, and that is where a task that consumes
    // its output without a mapping reads it.
    let workspace = Workspace::new()?;
    workspace.write(
        "app/env.cue",
        r#"package cuenv
name: "app"
tasks: {
	generate: {
		command: "sh"
		args: ["-c", "printf generated > data.txt"]
		dir: path: "tools"
		inputs: ["tools/template.txt"]
		outputs: ["data.txt"]
	}
	consume: {
		command: "sh"
		args: ["-c", "cp tools/data.txt consumed.txt"]
		inputs: [{task: "generate"}]
		outputs: ["consumed.txt"]
	}
}
"#,
    )?;
    workspace.write("app/tools/template.txt", "template\n")?;

    let output = workspace.task("app", &["consume"])?;

    assert_success(&output);
    assert_eq!(workspace.read("app/consumed.txt")?, "generated");
    Ok(())
}

#[test]
fn a_file_declared_twice_is_one_input() -> TestResult {
    // `src/**` covers the file `generate` wrote into `src/`, and the task
    // output names it again. It is staged once.
    let workspace = Workspace::new()?;
    workspace.write(
        "app/env.cue",
        r#"package cuenv
name: "app"
tasks: {
	generate: {
		command: "sh"
		args: ["-c", "printf generated > src/generated.ts"]
		inputs: ["src/main.ts"]
		outputs: ["src/generated.ts"]
	}
	build: {
		command: "sh"
		args: ["-c", "cat src/main.ts src/generated.ts > built.txt"]
		inputs: ["src/**", {task: "generate"}]
		outputs: ["built.txt"]
	}
}
"#,
    )?;
    workspace.write("app/src/main.ts", "main+")?;

    // The second run finds the generated file in the checkout, where the
    // glob also matches it.
    for _ in 0..2 {
        let output = workspace.task("app", &["build"])?;
        assert_success(&output);
        assert_eq!(workspace.read("app/built.txt")?, "main+generated");
    }
    Ok(())
}

#[test]
fn a_mapping_on_a_task_that_runs_in_the_workspace_is_an_error() -> TestResult {
    let workspace = Workspace::app_and_content()?;
    for opt_out in ["hermetic: false", "hermetic: sandbox: \"none\""] {
        workspace.write(
            "app/env.cue",
            &APP.replace(
                "outputs: [\"seen.md\", \"env.txt\"]",
                &format!("outputs: [\"seen.md\", \"env.txt\"]\n\t\t{opt_out}"),
            ),
        )?;

        let output = workspace.task("app", &["build"])?;

        assert_failure(&output);
        let printed = text(&output);
        assert!(printed.contains("nothing is staged"), "{printed}");
        assert!(printed.contains("vendor/one.md"), "{printed}");
        assert!(!workspace.exists("app/seen.md"), "{printed}");
    }
    Ok(())
}

#[test]
fn a_directory_that_does_not_evaluate_only_stops_references_by_name() -> TestResult {
    let workspace = Workspace::app_and_content()?;
    workspace.write("broken/env.cue", "package cuenv\nname: 12\ntasks: {")?;

    // A task that reaches no other project never lists the module.
    let unrelated = workspace.task("app", &["lint"])?;
    assert_success(&unrelated);
    assert_eq!(workspace.read("app/linted.txt")?, "linted");

    // A name could be carried by the directory that does not evaluate, so
    // the task that refers to a project by name fails, and says why.
    let by_name = workspace.task("app", &["build"])?;
    assert_failure(&by_name);
    let printed = text(&by_name);
    assert!(printed.contains("#content:articles"), "{printed}");
    assert!(
        printed.contains("Project 'content' could not be loaded"),
        "{printed}"
    );
    assert!(!workspace.exists("app/seen.md"), "{printed}");

    // A path names one directory whatever the rest of the module holds.
    workspace.write(
        "app/env.cue",
        &APP.replace("project: \"content\"", "project: \"../content\""),
    )?;
    let by_path = workspace.task("app", &["build"])?;
    assert_success(&by_path);
    assert_eq!(workspace.read("app/seen.md")?, "one\n");
    Ok(())
}

#[test]
fn a_group_runs_what_its_children_depend_on_in_another_project() -> TestResult {
    let workspace = Workspace::app_and_content()?;
    workspace.write(
        "app/env.cue",
        r#"package cuenv
name: "app"
tasks: {
	prepare: {
		command: "sh"
		args: ["-c", "printf prepared > prepared.txt"]
		inputs: ["src/**"]
		outputs: ["prepared.txt"]
	}
	checks: {
		type: "group"
		articles: {
			command: "sh"
			args: ["-c", "cp vendor/one.md seen.md"]
			inputs: [{project: "content", task: "articles", map: [{from: "out/one.md", to: "vendor/one.md"}]}]
			outputs: ["seen.md"]
		}
		local: {
			command: "sh"
			args: ["-c", "cp prepared.txt local.txt"]
			inputs: [{task: "prepare"}]
			outputs: ["local.txt"]
		}
	}
}
"#,
    )?;

    let output = workspace.task("app", &["checks"])?;

    assert_success(&output);
    assert_eq!(workspace.read("app/seen.md")?, "one\n");
    assert_eq!(workspace.read("app/local.txt")?, "prepared");
    Ok(())
}

#[test]
fn two_members_of_a_workspace_install_its_dependencies_once() -> TestResult {
    // Both projects are members of one bun workspace, so both get the
    // contributor's install step. A run that reaches both runs it once.
    let workspace = Workspace::new()?;
    workspace.write("package.json", r#"{"workspaces": ["app", "content"]}"#)?;
    for project in ["app", "content"] {
        workspace.write(&format!("{project}/package.json"), "{}")?;
        workspace.write(&format!("{project}/bun.lock"), "")?;
    }
    workspace.write(
        "app/env.cue",
        r#"package cuenv
name: "app"
tasks: build: {
	command: "bun"
	args: ["run", "build"]
	inputs: ["package.json", {project: "content", task: "articles", map: []}]
}
"#,
    )?;
    workspace.write(
        "content/env.cue",
        r#"package cuenv
name: "content"
tasks: articles: {
	command: "bun"
	args: ["run", "articles"]
	inputs: ["package.json"]
}
"#,
    )?;

    let output = workspace.task("app", &["build", "--dry-run"])?;

    assert_success(&output);
    let order = execution_order(&output)?;
    let installs: Vec<&String> = order
        .iter()
        .filter(|name| name.ends_with("bun.workspace.install"))
        .collect();
    assert_eq!(
        installs,
        ["cuenv.contributor.bun.workspace.install"],
        "{order:?}"
    );
    assert!(
        order.contains(&"#content:articles".to_string()),
        "{order:?}"
    );
    Ok(())
}
