//! End-to-end contracts for `cuenv ci` runs that have nothing to compare
//! with (issue 611).
//!
//! Affected detection needs a base. A run whose CI provider supplies none
//! runs its whole pipeline and says why. It never compares with the parent
//! of `HEAD` and reports "No affected tasks".

#[path = "../src/test_git.rs"]
mod test_git;

use gix::refs::transaction::PreviousValue;
use std::error::Error;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use tempfile::TempDir;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

const MODULE: &str = "module: \"test.example/no-base\"\nlanguage: version: \"v0.14.0\"\n";

/// Three tasks, each with one input file and a marker it leaves when it runs.
const PROJECT: &str = r#"package cuenv
name: "fixture"
let _tasks = tasks
ci: pipelines: default: tasks: [_tasks.first, _tasks.second, _tasks.untouched]
tasks: {
	first: {command: "sh", args: ["-c", "printf ran > first.ran"], inputs: ["first.txt"]}
	second: {command: "sh", args: ["-c", "printf ran > second.ran"], inputs: ["second.txt"]}
	untouched: {command: "sh", args: ["-c", "printf ran > untouched.ran"], inputs: ["untouched.txt"]}
}
"#;

const TASKS: [&str; 3] = ["first", "second", "untouched"];

/// The commit GitHub names when a ref did not exist before a push.
const NULL_SHA: &str = "0000000000000000000000000000000000000000";

/// A checkout of branch `main` with three commits. The second changes
/// `first.txt` and the third `second.txt`; nothing changes `untouched.txt`.
struct Checkout {
    directory: TempDir,
    base: String,
    middle: String,
    head: String,
}

impl Checkout {
    fn new() -> TestResult<Self> {
        let directory = tempfile::tempdir()?;
        let root = directory.path().join("checkout");
        fs::create_dir_all(root.join("cue.mod"))?;
        fs::write(root.join("cue.mod/module.cue"), MODULE)?;
        fs::write(root.join("env.cue"), PROJECT)?;
        for task in TASKS {
            fs::write(root.join(format!("{task}.txt")), "base")?;
        }
        test_git::init_repository(&root)?;
        let base = test_git::commit_all(&root, "base")?;
        fs::write(root.join("first.txt"), "changed")?;
        let middle = test_git::commit_all(&root, "the first input changes")?;
        fs::write(root.join("second.txt"), "changed")?;
        let head = test_git::commit_all(&root, "the second input changes")?;
        Ok(Self {
            directory,
            base,
            middle,
            head,
        })
    }

    fn root(&self) -> PathBuf {
        self.directory.path().join("checkout")
    }

    /// Point a ref at a commit, as a fetch or a push would have.
    fn point(&self, reference: &str, commit: &str) -> TestResult {
        let repository = gix::open(self.root())?;
        let commit = gix::ObjectId::from_hex(commit.as_bytes())?;
        repository.reference(reference, commit, PreviousValue::Any, "a test ref")?;
        Ok(())
    }

    /// Commit a merge of `parents` on `branch` and check it out. Its tree is
    /// the tree of `HEAD`, so the worktree already matches it.
    fn merge(&self, branch: &str, parents: [&str; 2]) -> TestResult<String> {
        let repository = gix::open(self.root())?;
        let tree = repository.head_commit()?.tree_id()?.detach();
        let parents = parents
            .iter()
            .map(|parent| gix::ObjectId::from_hex(parent.as_bytes()))
            .collect::<Result<Vec<_>, _>>()?;
        let merge = repository.new_commit("a merge", tree, parents)?.id;
        let reference = format!("refs/heads/{branch}");
        repository.reference(
            reference.as_str(),
            merge,
            PreviousValue::Any,
            "a test merge",
        )?;
        fs::write(self.root().join(".git/HEAD"), format!("ref: {reference}\n"))?;
        Ok(merge.to_string())
    }

    /// Run `cuenv ci` in an environment that holds nothing of the CI system
    /// these tests themselves run on, and so detects no CI provider unless
    /// `environment` describes one.
    fn ci(&self, environment: &[(&str, &str)], arguments: &[&str]) -> TestResult<Run> {
        let mut command = Command::new(env!("CARGO_BIN_EXE_cuenv"));
        command.current_dir(self.root());
        for (name, _) in std::env::vars_os() {
            let name_text = name.to_string_lossy();
            if name_text == "CI"
                || name_text == "GH_TOKEN"
                || name_text.starts_with("GITHUB_")
                || name_text.starts_with("BUILDKITE")
            {
                command.env_remove(&name);
            }
        }
        let output = command
            .env("CUENV_STATE_DIR", self.directory.path().join("state"))
            .env("CUENV_CACHE_DIR", self.directory.path().join("cache"))
            .envs(environment.iter().copied())
            .arg("ci")
            .args(arguments)
            .output()?;
        let ran = TASKS
            .into_iter()
            .filter(|task| self.root().join(format!("{task}.ran")).exists())
            .collect();
        Ok(Run {
            printed: text(&output),
            succeeded: output.status.success(),
            ran,
        })
    }

    /// Run `cuenv ci` as a GitHub Actions run of `event` on `git_ref`, with
    /// `payload` as the event GitHub delivered.
    fn github(&self, event: &str, git_ref: &str, payload: &serde_json::Value) -> TestResult<Run> {
        let event_path = self.directory.path().join("event.json");
        fs::write(&event_path, serde_json::to_vec(payload)?)?;
        let event_path = event_path.to_str().ok_or("a path that is not UTF-8")?;
        let ref_name = git_ref.rsplit('/').next().unwrap_or(git_ref);
        let head = self.head_sha()?;
        self.ci(
            &[
                ("CI", "true"),
                ("GITHUB_ACTIONS", "true"),
                ("GITHUB_REPOSITORY", "example/fixture"),
                ("GITHUB_EVENT_NAME", event),
                ("GITHUB_EVENT_PATH", event_path),
                ("GITHUB_REF", git_ref),
                ("GITHUB_REF_NAME", ref_name),
                ("GITHUB_SHA", &head),
                (
                    "GITHUB_BASE_REF",
                    if event.starts_with("pull_request") {
                        "main"
                    } else {
                        ""
                    },
                ),
            ],
            &[],
        )
    }

    /// Run `cuenv ci` as a Buildkite build, with `variables` on top of what
    /// Buildkite sets for every build of a pipeline whose default branch is
    /// `main`.
    fn buildkite(&self, variables: &[(&str, &str)]) -> TestResult<Run> {
        let head = self.head_sha()?;
        let mut environment = vec![
            ("CI", "true"),
            ("BUILDKITE", "true"),
            ("BUILDKITE_COMMIT", head.as_str()),
            ("BUILDKITE_PIPELINE_DEFAULT_BRANCH", "main"),
            ("BUILDKITE_SOURCE", "webhook"),
            ("BUILDKITE_PULL_REQUEST", "false"),
            ("BUILDKITE_PULL_REQUEST_BASE_BRANCH", ""),
            ("BUILDKITE_TAG", ""),
        ];
        // A later variable of the same name wins, as it does in a shell.
        environment.extend_from_slice(variables);
        self.ci(&environment, &[])
    }

    fn head_sha(&self) -> TestResult<String> {
        Ok(gix::open(self.root())?.head_id()?.to_string())
    }

    /// The report a run under a CI provider wrote. The module has one
    /// project, so there is one.
    fn report(&self) -> TestResult<serde_json::Value> {
        let reports = self.root().join(".cuenv/reports").join(self.head_sha()?);
        let paths = fs::read_dir(reports)?.collect::<Result<Vec<_>, _>>()?;
        let [report] = paths.as_slice() else {
            return Err(format!("expected one report, found {}", paths.len()).into());
        };
        Ok(serde_json::from_slice(&fs::read(report.path())?)?)
    }
}

/// What a run of `cuenv ci` did.
struct Run {
    /// Everything the run printed, on one line.
    printed: String,
    succeeded: bool,
    /// The tasks that ran, in pipeline order.
    ran: Vec<&'static str>,
}

impl Run {
    /// The run had no base: it ran every task and said why.
    fn assert_ran_the_whole_pipeline_for(&self, reason: &str) {
        assert!(self.succeeded, "{}", self.printed);
        assert_eq!(self.ran, TASKS, "{}", self.printed);
        let line = format!("No comparable base ({reason}): running the whole pipeline");
        assert!(self.printed.contains(&line), "{}", self.printed);
        assert!(
            !self.printed.contains("No affected tasks"),
            "{}",
            self.printed
        );
    }

    /// The run was compared with a base and ran only what that showed to be
    /// affected.
    fn assert_ran_only(&self, tasks: &[&str]) {
        assert!(self.succeeded, "{}", self.printed);
        assert_eq!(self.ran, tasks, "{}", self.printed);
        assert!(
            !self.printed.contains("No comparable base"),
            "{}",
            self.printed
        );
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

fn assert_only_unrecognised_ci_mentions_from(run: &Run) {
    // `--from` is ignored under a detected provider, so naming it there
    // would send the user after a flag that does nothing.
    assert!(!run.printed.contains("--from"), "{}", run.printed);
}

#[test]
fn a_scheduled_run_runs_the_whole_pipeline() -> TestResult {
    let checkout = Checkout::new()?;

    let run = checkout.github(
        "schedule",
        "refs/heads/main",
        &serde_json::json!({"schedule": "0 4 * * *"}),
    )?;

    run.assert_ran_the_whole_pipeline_for("GitHub schedule event");
    assert_only_unrecognised_ci_mentions_from(&run);
    // The report says the run was not compared, not that nothing changed.
    let report = checkout.report()?;
    assert_eq!(report["context"]["no_base"], "GitHub schedule event");
    assert_eq!(report["context"]["changed_files"], serde_json::json!([]));
    Ok(())
}

#[test]
fn a_manually_dispatched_run_runs_the_whole_pipeline() -> TestResult {
    let checkout = Checkout::new()?;

    let run = checkout.github(
        "workflow_dispatch",
        "refs/heads/main",
        &serde_json::json!({"inputs": {}}),
    )?;

    run.assert_ran_the_whole_pipeline_for("GitHub workflow_dispatch event");
    Ok(())
}

#[test]
fn a_release_still_runs_the_whole_pipeline() -> TestResult {
    // A release always ran everything. It does so now because the provider
    // reports that it has no base, as any other run without one does.
    let checkout = Checkout::new()?;

    let run = checkout.github(
        "release",
        "refs/tags/1.0.0",
        &serde_json::json!({"action": "published", "release": {"tag_name": "1.0.0"}}),
    )?;

    run.assert_ran_the_whole_pipeline_for("GitHub release event");
    Ok(())
}

#[test]
fn a_tag_push_runs_the_whole_pipeline() -> TestResult {
    let checkout = Checkout::new()?;

    let run = checkout.github(
        "push",
        "refs/tags/1.0.0",
        &serde_json::json!({"before": NULL_SHA, "ref": "refs/tags/1.0.0"}),
    )?;

    run.assert_ran_the_whole_pipeline_for("GitHub tag push");
    Ok(())
}

#[test]
fn the_first_push_of_a_branch_runs_the_whole_pipeline() -> TestResult {
    let checkout = Checkout::new()?;

    let run = checkout.github(
        "push",
        "refs/heads/feature",
        &serde_json::json!({"before": NULL_SHA, "created": true}),
    )?;

    run.assert_ran_the_whole_pipeline_for("first push of a GitHub branch");
    Ok(())
}

#[test]
fn a_push_whose_previous_commit_cannot_be_compared_runs_the_whole_pipeline() -> TestResult {
    // The commit is not in the checkout, and without a token neither the
    // compare API nor a fetch can supply it.
    let checkout = Checkout::new()?;

    let run = checkout.github(
        "push",
        "refs/heads/main",
        &serde_json::json!({"before": "1234567890123456789012345678901234567890"}),
    )?;

    run.assert_ran_the_whole_pipeline_for(
        "GitHub push whose previous commit 1234567 cannot be compared",
    );
    assert_only_unrecognised_ci_mentions_from(&run);
    Ok(())
}

#[test]
fn a_push_is_compared_with_the_commit_before_it() -> TestResult {
    // One push holding two commits: both are compared, not only the last.
    let checkout = Checkout::new()?;

    let run = checkout.github(
        "push",
        "refs/heads/main",
        &serde_json::json!({"before": checkout.base}),
    )?;

    run.assert_ran_only(&["first", "second"]);
    Ok(())
}

#[test]
fn a_merge_queue_run_is_compared_with_the_base_of_its_merge_group() -> TestResult {
    let checkout = Checkout::new()?;

    let run = checkout.github(
        "merge_group",
        "refs/heads/gh-readonly-queue/main/pr-7-abc",
        &serde_json::json!({"merge_group": {
            "base_sha": checkout.middle,
            "base_ref": "refs/heads/main",
            "head_sha": checkout.head,
        }}),
    )?;

    run.assert_ran_only(&["second"]);
    assert!(run.printed.contains("Changed files: 1"), "{}", run.printed);
    let report = checkout.report()?;
    assert_eq!(report["context"].get("no_base"), None);
    assert_eq!(
        report["context"]["changed_files"],
        serde_json::json!(["second.txt"])
    );
    Ok(())
}

#[test]
fn a_github_pull_request_is_compared_with_its_base() -> TestResult {
    // GitHub checks out its merge of the pull request into the base. The
    // first parent of that merge is the base, so `HEAD^1` is not a guess.
    let checkout = Checkout::new()?;
    checkout.merge("pull-request-merge", [&checkout.middle, &checkout.head])?;

    let run = checkout.github(
        "pull_request",
        "refs/pull/7/merge",
        &serde_json::json!({"pull_request": {"base": {"sha": checkout.middle}}}),
    )?;

    run.assert_ran_only(&["second"]);
    Ok(())
}

#[test]
fn a_pull_request_target_run_on_the_base_branch_runs_the_whole_pipeline() -> TestResult {
    // `pull_request_target` checks out the base branch, which differs from
    // itself in nothing: comparing the checkout would report "No affected
    // tasks" for every pull request. Only GitHub can say what the pull
    // request changes, and without a token it is not asked.
    let checkout = Checkout::new()?;
    checkout.point("refs/remotes/origin/main", &checkout.head)?;

    let run = checkout.github(
        "pull_request_target",
        "refs/heads/main",
        &serde_json::json!({"pull_request": {
            "number": 7,
            "base": {"sha": checkout.head},
            "head": {"sha": "1234567890123456789012345678901234567890"},
        }}),
    )?;

    run.assert_ran_the_whole_pipeline_for(
        "GitHub pull request that is not checked out and whose files could not be listed",
    );
    assert_only_unrecognised_ci_mentions_from(&run);
    Ok(())
}

#[test]
fn a_pull_request_target_run_that_checks_out_the_head_is_compared_with_its_base() -> TestResult {
    // The workflow checked out the pull request's head itself, so the
    // checkout does say what the pull request changes.
    let checkout = Checkout::new()?;
    checkout.point("refs/remotes/origin/main", &checkout.middle)?;

    let run = checkout.github(
        "pull_request_target",
        "refs/heads/main",
        &serde_json::json!({"pull_request": {
            "number": 7,
            "base": {"sha": checkout.middle},
            "head": {"sha": checkout.head},
        }}),
    )?;

    run.assert_ran_only(&["second"]);
    Ok(())
}

#[test]
fn a_pull_request_run_on_another_checkout_runs_the_whole_pipeline() -> TestResult {
    // The run is for a pull request's ref, and the workflow checked out a
    // commit that is not the pull request's head.
    let checkout = Checkout::new()?;
    checkout.point("refs/remotes/origin/main", &checkout.head)?;

    let run = checkout.github(
        "pull_request",
        "refs/pull/7/head",
        &serde_json::json!({"pull_request": {
            "number": 7,
            "base": {"sha": checkout.middle},
            "head": {"sha": "1234567890123456789012345678901234567890"},
        }}),
    )?;

    run.assert_ran_the_whole_pipeline_for(
        "GitHub pull request that is not checked out and whose files could not be listed",
    );
    Ok(())
}

#[test]
fn a_buildkite_build_started_from_the_ui_runs_the_whole_pipeline() -> TestResult {
    let checkout = Checkout::new()?;

    let run = checkout.buildkite(&[("BUILDKITE_BRANCH", "main"), ("BUILDKITE_SOURCE", "ui")])?;

    run.assert_ran_the_whole_pipeline_for("Buildkite branch build");
    assert_only_unrecognised_ci_mentions_from(&run);
    Ok(())
}

#[test]
fn a_buildkite_build_of_a_push_holding_two_commits_runs_the_tasks_of_both() -> TestResult {
    // `main` gained two commits in one push. The parent of `HEAD` is the
    // first of them, so comparing with it would leave `first` out.
    let checkout = Checkout::new()?;
    checkout.point("refs/remotes/origin/main", &checkout.head)?;

    let run = checkout.buildkite(&[("BUILDKITE_BRANCH", "main")])?;

    run.assert_ran_the_whole_pipeline_for("Buildkite branch build");
    Ok(())
}

#[test]
fn a_buildkite_branch_that_merged_the_default_branch_in_runs_the_whole_pipeline() -> TestResult {
    // `staging` was at the first commit and merges `main` in. Compared with
    // the default branch it holds nothing new, yet everything `main` gained
    // is new to `staging`.
    let checkout = Checkout::new()?;
    checkout.point("refs/remotes/origin/main", &checkout.head)?;
    checkout.merge("staging", [&checkout.base, &checkout.head])?;

    let run = checkout.buildkite(&[("BUILDKITE_BRANCH", "staging")])?;

    run.assert_ran_the_whole_pipeline_for("Buildkite branch build");
    Ok(())
}

#[test]
fn a_buildkite_promotion_runs_the_whole_pipeline() -> TestResult {
    // `git push origin main:production` builds the very commit the default
    // branch is at, so the two branches do not differ.
    let checkout = Checkout::new()?;
    checkout.point("refs/remotes/origin/main", &checkout.head)?;

    let run = checkout.buildkite(&[("BUILDKITE_BRANCH", "production")])?;

    run.assert_ran_the_whole_pipeline_for("Buildkite branch build");
    Ok(())
}

#[test]
fn a_buildkite_tag_build_runs_the_whole_pipeline() -> TestResult {
    // A tag on the default branch's tip.
    let checkout = Checkout::new()?;
    checkout.point("refs/remotes/origin/main", &checkout.head)?;

    let run = checkout.buildkite(&[("BUILDKITE_BRANCH", "1.0.0"), ("BUILDKITE_TAG", "1.0.0")])?;

    run.assert_ran_the_whole_pipeline_for("Buildkite tag build");
    Ok(())
}

#[test]
fn a_buildkite_pull_request_is_compared_with_its_base_branch() -> TestResult {
    let checkout = Checkout::new()?;
    checkout.point("refs/remotes/origin/main", &checkout.middle)?;

    let run = checkout.buildkite(&[
        ("BUILDKITE_BRANCH", "feature"),
        ("BUILDKITE_PULL_REQUEST", "7"),
        ("BUILDKITE_PULL_REQUEST_BASE_BRANCH", "main"),
    ])?;

    run.assert_ran_only(&["second"]);
    Ok(())
}

#[test]
fn a_ci_system_cuenv_does_not_recognise_runs_the_whole_pipeline() -> TestResult {
    let checkout = Checkout::new()?;

    let run = checkout.ci(&[("CI", "true"), ("GITLAB_CI", "true")], &[])?;

    run.assert_ran_the_whole_pipeline_for(
        "`CI` is set and cuenv does not recognise this CI system",
    );
    assert!(
        run.printed
            .contains("Pass --from <ref> to compare with that ref instead"),
        "{}",
        run.printed
    );
    Ok(())
}

#[test]
fn from_gives_a_ci_system_cuenv_does_not_recognise_its_base() -> TestResult {
    let checkout = Checkout::new()?;

    let run = checkout.ci(&[("CI", "true")], &["--from", &checkout.middle])?;

    run.assert_ran_only(&["second"]);
    Ok(())
}

#[test]
fn a_local_run_still_compares_with_head() -> TestResult {
    // Nothing is uncommitted, so nothing is affected: with `CI` unset, or
    // set to a spelling of "no", this is a local run and not one without a
    // base.
    for environment in [&[][..], &[("CI", "false")], &[("CI", "0")], &[("CI", "")]] {
        let checkout = Checkout::new()?;

        let run = checkout.ci(environment, &[])?;

        run.assert_ran_only(&[]);
        assert!(run.printed.contains("No affected tasks"), "{}", run.printed);
    }
    Ok(())
}

#[test]
fn a_local_run_outside_any_repository_runs_the_whole_pipeline() -> TestResult {
    let checkout = Checkout::new()?;
    fs::remove_dir_all(checkout.root().join(".git"))?;

    let run = checkout.ci(&[], &[])?;

    run.assert_ran_the_whole_pipeline_for("this directory is not inside a git repository");
    Ok(())
}
