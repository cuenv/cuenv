//! A task whose fields CUE adds in a disjunct it later supersedes still
//! evaluates in bounded time.
//!
//! A node with two disjunctions is cloned for the first and cloned again for
//! the second. The fields added to the first clone keep it as the
//! environment of their conjuncts, and evaluation leaves it unfinished. The
//! bridge records the reference of every field through `Value.Expr`, which
//! evaluated such a field's operands in that clone again: it processed the
//! clone's disjunctions, and those of every task its dependencies reach,
//! without the sharing that evaluation used. A single dependency ran out of
//! 8 GB, while `cue export` of the same project takes seconds. The CUE fork
//! now evaluates the operands where the value settled.
//!
//! Two declarations give a task two disjunctions: a matrix entry over a task
//! reached through a `let` (`#TaskNode` from `#MatrixTask.task`, and the
//! pattern of `tasks` that the `let` brings along), and a task typed with
//! `schema.#TaskNode` (that union, and the pattern of `tasks`).

use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};
use std::{fs, thread};
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

/// The module is the schema's own, so the fixture imports the real schema
/// copied into it.
const MODULE: &str = "module: \"github.com/cuenv/cuenv\"\nlanguage: version: \"v0.14.1\"\n";

/// Tasks in the chain behind `build`. Before the fix one was enough to run
/// out of memory; the chain makes an exponential regression unmistakable.
const DEPTH: usize = 8;

/// The bound only catches a regression, never a slow machine: evaluation
/// takes well under a second. A regression grows memory by gigabytes a
/// second, so the run is stopped rather than awaited.
const BOUND: Duration = Duration::from_secs(30);

/// A module with the schema and one project, `app`, whose `build` is
/// declared by `build` and depends on a chain of `DEPTH` tasks.
fn project(build: &str, declarations: &str) -> TestResult<TempDir> {
    let directory = tempfile::Builder::new().prefix("cuenv_test_").tempdir()?;
    let root = directory.path();
    write(root, "cue.mod/module.cue", MODULE)?;
    copy_cue_files(
        &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../schema"),
        &root.join("schema"),
    )?;
    let chain: String = (1..=DEPTH)
        .map(|index| {
            let next = index + 1;
            if index < DEPTH {
                format!("tasks: t{index}: {{command: \"true\", dependsOn: [tasks.t{next}]}}\n")
            } else {
                format!("tasks: t{index}: {{command: \"true\"}}\n")
            }
        })
        .collect();
    let source = format!(
        "package cuenv\n\nimport \"github.com/cuenv/cuenv/schema\"\n\nschema.#Project\nname: \
         \"app\"\n{declarations}\ntasks: build: {build} {{command: \"true\", dependsOn: \
         [tasks.t1]}}\n{chain}"
    );
    write(root, "app/env.cue", &source)?;
    Ok(directory)
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

fn write(root: &Path, relative: &str, content: &str) -> TestResult {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, content)?;
    Ok(())
}

/// `cuenv task build --dry-run` in `app`, stopped once it exceeds `BOUND`.
fn plan_build(module: &TempDir) -> TestResult<Output> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_cuenv"))
        .current_dir(module.path().join("app"))
        .env_clear()
        .env("PATH", std::env::var("PATH").unwrap_or_default())
        .env("HOME", std::env::var("HOME").unwrap_or_default())
        .env("CUENV_STATE_DIR", module.path().join("state"))
        .args(["task", "build", "--dry-run"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let started = Instant::now();
    while child.try_wait()?.is_none() {
        if started.elapsed() > BOUND {
            child.kill()?;
            child.wait()?;
            return Err(format!("evaluation did not finish within {BOUND:?}").into());
        }
        thread::sleep(Duration::from_millis(20));
    }
    Ok(child.wait_with_output()?)
}

/// The order the dry run would run tasks in.
fn execution_order(output: &Output) -> TestResult<Vec<String>> {
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let plan: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    Ok(plan["execution_order"]
        .as_array()
        .ok_or("a dry run lists its execution order")?
        .iter()
        .filter_map(serde_json::Value::as_str)
        .map(str::to_string)
        .collect())
}

fn assert_plans_the_chain(order: &[String]) {
    assert_eq!(order.len(), DEPTH + 1, "{order:?}");
    let deepest = format!("t{DEPTH}");
    assert_eq!(order.first(), Some(&deepest), "{order:?}");
    assert_eq!(order.last().map(String::as_str), Some("build"), "{order:?}");
}

#[test]
fn a_matrix_over_a_task_reached_through_a_let_evaluates() -> TestResult {
    // `tasks` in the pipeline would name the pipeline's own list.
    let module = project(
        "",
        "let _tasks = tasks\nci: providers: [\"github\"]\nci: pipelines: default: {tasks: \
         [{type: \"matrix\", task: _tasks.build, matrix: arch: [\"x\", \"y\"]}], when: branch: \
         \"main\"}",
    )?;

    let order = execution_order(&plan_build(&module)?)?;

    assert_plans_the_chain(&order);
    Ok(())
}

#[test]
fn a_task_typed_as_a_task_node_evaluates() -> TestResult {
    let module = project("schema.#TaskNode &", "")?;

    let order = execution_order(&plan_build(&module)?)?;

    assert_plans_the_chain(&order);
    Ok(())
}
