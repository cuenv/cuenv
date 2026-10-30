//! End-to-end contracts for complete pipeline execution without event rewriting.

use std::error::Error;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

struct Fixture {
    directory: TempDir,
    base: String,
    head: String,
}

impl Fixture {
    fn new(source: &str) -> TestResult<Self> {
        let directory = tempfile::tempdir()?;
        let root = directory.path();
        fs::create_dir(root.join("cue.mod"))?;
        fs::write(
            root.join("cue.mod/module.cue"),
            "module: \"test.example/ci-all\"\nlanguage: version: \"v0.14.0\"\n",
        )?;
        fs::write(root.join("env.cue"), source)?;
        fs::write(root.join("untouched.txt"), "same")?;
        fs::write(root.join("changed.txt"), "base")?;
        git(root, &["init", "-q"])?;
        git(root, &["config", "user.name", "CI contracts"])?;
        git(root, &["config", "user.email", "ci@example.test"])?;
        git(root, &["add", "."])?;
        git(root, &["commit", "-qm", "base"])?;
        let base = git(root, &["rev-parse", "HEAD"])?;
        fs::write(root.join("changed.txt"), "head")?;
        git(root, &["add", "changed.txt"])?;
        git(root, &["commit", "-qm", "head"])?;
        let head = git(root, &["rev-parse", "HEAD"])?;
        Ok(Self {
            directory,
            base,
            head,
        })
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cuenv"));
        command
            .current_dir(self.directory.path())
            .env_remove("GITHUB_ACTIONS")
            .env_remove("BUILDKITE")
            .env_remove("GITHUB_TOKEN")
            .env_remove("GH_TOKEN")
            .env("CUENV_STATE_DIR", self.directory.path().join("state"));
        command
    }

    fn run(&self, arguments: &[&str]) -> TestResult<Output> {
        Ok(self.command().args(arguments).output()?)
    }
}

fn git(root: &Path, arguments: &[&str]) -> TestResult<String> {
    let output = Command::new("git")
        .args([
            "-c",
            "commit.gpgsign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(arguments)
        .current_dir(root)
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn all_runs_unaffected_tasks_and_dependencies_while_default_still_filters() -> TestResult {
    let fixture = Fixture::new(
        r#"package cuenv
name: "fixture"
let _tasks = tasks
ci: pipelines: verify: tasks: [_tasks.check, _tasks.always]
tasks: {
  prepare: {command: "sh", args: ["-c", "printf before > trace"], inputs: ["untouched.txt"]}
  check: {command: "sh", args: ["-c", "printf after >> trace"], inputs: ["untouched.txt"], dependsOn: [_tasks.prepare]}
  always: {command: "sh", args: ["-c", "printf ran > always-ran"]}
}
"#,
    )?;
    assert_success(&fixture.run(&["ci", "--pipeline", "verify", "--from", &fixture.base])?);
    assert!(!fixture.directory.path().join("trace").exists());
    assert!(fixture.directory.path().join("always-ran").exists());
    assert_success(&fixture.run(&[
        "ci",
        "--pipeline",
        "verify",
        "--from",
        &fixture.base,
        "--all",
    ])?);
    assert_eq!(
        fs::read_to_string(fixture.directory.path().join("trace"))?,
        "beforeafter"
    );
    Ok(())
}

#[test]
fn all_propagates_a_real_task_failure() -> TestResult {
    let fixture = Fixture::new(
        r#"package cuenv
name: "fixture"
let _tasks = tasks
ci: pipelines: verify: tasks: [_tasks.reject]
tasks: reject: {command: "sh", args: ["-c", "exit 7"], inputs: ["untouched.txt"]}
"#,
    )?;
    let output = fixture.run(&["ci", "--pipeline", "verify", "--all"])?;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("reject failed"));
    Ok(())
}

#[test]
fn all_keeps_the_real_provider_event_in_hooks_and_reports() -> TestResult {
    let fixture = Fixture::new(
        r#"package cuenv
name: "fixture"
let _tasks = tasks
hooks: onEnter: capture: {
  command: "sh"
  args: ["-c", "printf 'export OBSERVED_EVENT=%s\\n' \"$GITHUB_EVENT_NAME\""]
  source: true
}
ci: pipelines: verify: tasks: [_tasks.check]
tasks: check: {command: "sh", args: ["-c", "test \"$OBSERVED_EVENT\" = merge_group && printf checked > event-checked"], inputs: ["untouched.txt"]}
"#,
    )?;
    let output = fixture
        .command()
        .args(["ci", "--pipeline", "verify", "--all"])
        .env("GITHUB_ACTIONS", "true")
        .env("GITHUB_EVENT_NAME", "merge_group")
        .env("GITHUB_REPOSITORY", "example/fixture")
        .env("GITHUB_REF", "refs/heads/gh-readonly-queue/main/candidate")
        .env("GITHUB_REF_NAME", "gh-readonly-queue/main/candidate")
        .env("GITHUB_SHA", &fixture.head)
        .env("GITHUB_BASE_REF", "")
        .env_remove("GITHUB_BEFORE")
        .output()?;
    assert_success(&output);
    assert!(fixture.directory.path().join("event-checked").exists());
    let reports = fixture
        .directory
        .path()
        .join(".cuenv/reports")
        .join(&fixture.head);
    let paths = fs::read_dir(reports)?.collect::<Result<Vec<_>, _>>()?;
    assert_eq!(paths.len(), 1);
    let report: serde_json::Value = serde_json::from_slice(&fs::read(paths[0].path())?)?;
    assert_eq!(report["context"]["event"], "merge_group");
    assert_eq!(report["context"]["sha"], fixture.head);
    assert_eq!(report["tasks"][0]["name"], "check");
    assert_eq!(report["tasks"][0]["exit_code"], 0);
    Ok(())
}

#[test]
fn all_rejects_missing_projects_empty_pipelines_and_inline_tasks() -> TestResult {
    let fixture =
        Fixture::new("package cuenv\nname: \"fixture\"\nci: pipelines: verify: tasks: []\n")?;
    let root = fixture.directory.path();
    fs::create_dir(root.join("empty"))?;
    for arguments in [
        vec!["ci", "--pipeline", "verify", "--path", "empty", "--all"],
        vec!["ci", "--pipeline", "missing", "--all"],
        vec!["ci", "--pipeline", "verify", "--all"],
    ] {
        assert!(!fixture.run(&arguments)?.status.success());
    }
    fs::write(
        root.join("env.cue"),
        r#"package cuenv
name: "fixture"
ci: pipelines: verify: tasks: [{command: "false", description: "safe"}]
tasks: safe: {command: "true"}
"#,
    )?;
    let output = fixture.run(&["ci", "--pipeline", "verify", "--all"])?;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("named task references"));
    fs::write(root.join("env.cue"), "package cuenv\n")?;
    assert!(
        !fixture
            .run(&["ci", "--pipeline", "verify", "--all"])?
            .status
            .success()
    );
    Ok(())
}

#[test]
fn all_preserves_ci_execution_for_declared_sibling_inputs() -> TestResult {
    let fixture = Fixture::new("package cuenv\n")?;
    let root = fixture.directory.path();
    fs::create_dir(root.join("web"))?;
    fs::write(root.join("package.json"), "{}")?;
    fs::write(
        root.join("web/env.cue"),
        r#"package cuenv
name: "web"
let _tasks = tasks
ci: pipelines: verify: tasks: [_tasks.check]
tasks: check: {
  command: "sh"
  args: ["-c", "test -r ../package.json && printf checked > checked"]
  inputs: ["../package.json"]
  hermetic: sandbox: "dir"
}
"#,
    )?;
    assert_success(&fixture.run(&["ci", "--pipeline", "verify", "--path", "web", "--all"])?);
    assert!(root.join("web/checked").exists());
    Ok(())
}
