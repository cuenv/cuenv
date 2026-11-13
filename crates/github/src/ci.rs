//! GitHub Actions CI provider.
//!
//! This provider integrates with GitHub Actions to:
//! - Compare a run with the base GitHub supplies for it: a pull request's
//!   base, the commit a push came after, or the commit a merge group is
//!   built on
//! - Create and update check runs
//! - Post PR comments with pipeline reports
//!
//! A run GitHub supplies no base for (a schedule, a manual dispatch, a tag,
//! the first push of a branch, a release) is compared with nothing: the
//! parent of `HEAD` is not what such a run came after.

use async_trait::async_trait;
use cuenv_ci::context::CIContext;
use cuenv_ci::provider::git::{Fetch, RunCheckout, Since};
use cuenv_ci::provider::{CIProvider, ChangedFiles, NoBase};
use cuenv_ci::report::{CheckHandle, PipelineReport, PipelineStatus, markdown};
use cuenv_core::Result;
use octocrab::Octocrab;
use octocrab::models::repos::DiffEntry;
use std::path::PathBuf;
use tracing::{debug, info, warn};

/// GitHub Actions CI provider.
///
/// Provides CI integration for repositories hosted on GitHub using GitHub Actions.
pub struct GitHubCIProvider {
    context: CIContext,
    token: String,
    owner: String,
    repo: String,
    /// `GITHUB_REF`: the full ref the run is for.
    git_ref: String,
    pr_number: Option<u64>,
    checkout: RunCheckout,
}

const NULL_SHA: &str = "0000000000000000000000000000000000000000";

/// A GitHub API that lists changed files, and the most files it lists
/// however many changed. A listing that long may have been cut short, and a
/// list that may be incomplete is not a comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileListing {
    api: &'static str,
    limit: usize,
}

/// `GET /repos/{owner}/{repo}/compare/{base}...{head}`.
const COMPARE_API: FileListing = FileListing {
    api: "Compare API",
    limit: 300,
};

/// `GET /repos/{owner}/{repo}/pulls/{number}/files`, over all of its pages.
const PULL_REQUEST_FILES_API: FileListing = FileListing {
    api: "pull request files API",
    limit: 3000,
};

impl FileListing {
    /// The entries of a listing, unless the listing may have been cut short.
    fn complete<Entry>(self, entries: Vec<Entry>) -> Result<Vec<Entry>> {
        if entries.len() >= self.limit {
            return Err(cuenv_core::Error::configuration(format!(
                "GitHub's {} lists at most {} changed files and returned {}, so the listing \
                 may be incomplete",
                self.api,
                self.limit,
                entries.len()
            )));
        }
        Ok(entries)
    }
}

/// What GitHub supplies for a run to compare itself with.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SuppliedBase {
    /// The run is for a pull request, whose base is the branch it targets.
    PullRequest,
    /// A push: the commit the ref was at before.
    PushBefore(String),
    /// A merge queue run: the commit the merge group is built on.
    MergeGroupBase(String),
    /// Nothing: the event names no commit this run came after.
    Nothing(NoBase),
}

/// A commit hash short enough to read in a message.
fn short_sha(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

/// The paths an entry of an API listing stands for. A renamed file is under
/// both of its names, as the checkout's own comparison reports it: the
/// project it left has changed as much as the one it joined.
fn listed_paths(entry: &DiffEntry) -> impl Iterator<Item = PathBuf> + '_ {
    entry
        .previous_filename
        .iter()
        .chain(std::iter::once(&entry.filename))
        .map(PathBuf::from)
}

impl GitHubCIProvider {
    fn parse_repo(repo_str: &str) -> (String, String) {
        let parts: Vec<&str> = repo_str.split('/').collect();
        if parts.len() == 2 {
            (parts[0].to_string(), parts[1].to_string())
        } else {
            (String::new(), String::new())
        }
    }

    /// Extract PR number from `GITHUB_REF` (e.g., "refs/pull/123/merge" -> 123)
    fn parse_pr_number(github_ref: &str) -> Option<u64> {
        if github_ref.starts_with("refs/pull/") {
            github_ref
                .strip_prefix("refs/pull/")?
                .split('/')
                .next()?
                .parse()
                .ok()
        } else {
            None
        }
    }

    /// Whether the run is for `refs/pull/<number>/merge`, GitHub's merge of
    /// a pull request into its base.
    fn is_pr_merge_ref(&self) -> bool {
        self.git_ref.starts_with("refs/pull/") && self.git_ref.ends_with("/merge")
    }

    /// Whether the checkout holds the pull request: its head, or GitHub's
    /// merge of it into its base. Only then does comparing the checkout with
    /// the base say what the pull request changes. Any other checkout says
    /// nothing about it: `pull_request_target` checks out the base branch,
    /// which differs from itself in nothing.
    fn checkout_holds_pull_request(&self, event: &serde_json::Value) -> bool {
        let holds_merge = self.is_pr_merge_ref() && self.is_synthetic_merge_checkout();
        let holds_head = Self::commit_at(event, "/pull_request/head/sha")
            .is_some_and(|head| self.checkout.head_is(&head));
        holds_merge || holds_head
    }

    /// The pull request's number: from the ref the run is for, or from the
    /// event when the run is for the base branch's ref, as a
    /// `pull_request_target` run is.
    fn pull_request_number(&self, event: &serde_json::Value) -> Option<u64> {
        self.pr_number.or_else(|| {
            event
                .pointer("/pull_request/number")
                .and_then(serde_json::Value::as_u64)
        })
    }

    fn is_synthetic_merge_checkout(&self) -> bool {
        !self.context.sha.is_empty() && self.checkout.head_is(&self.context.sha)
    }

    /// The payload of the event that started the run; `Null` when it cannot
    /// be read.
    fn event_payload() -> serde_json::Value {
        std::env::var("GITHUB_EVENT_PATH")
            .ok()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or(serde_json::Value::Null)
    }

    /// What GitHub supplies for this run to compare itself with.
    ///
    /// `explicit_before` is `GITHUB_BEFORE`, with which a workflow names the
    /// previous commit itself; it stands in for the payload's `before`.
    fn supplied_base(
        &self,
        event: &serde_json::Value,
        explicit_before: Option<&str>,
    ) -> SuppliedBase {
        let targets_a_branch = self
            .context
            .base_ref
            .as_deref()
            .is_some_and(|base| !base.is_empty());
        if self.pr_number.is_some() || targets_a_branch {
            return SuppliedBase::PullRequest;
        }

        if let Some(base_sha) = Self::parse_merge_group_base_sha(event) {
            return SuppliedBase::MergeGroupBase(base_sha);
        }

        let before =
            explicit_before.or_else(|| event.get("before").and_then(serde_json::Value::as_str));
        match before {
            // GitHub names the null commit when the ref did not exist.
            Some(NULL_SHA) => {
                SuppliedBase::Nothing(NoBase::new(if self.git_ref.starts_with("refs/tags/") {
                    "GitHub tag push"
                } else {
                    "first push of a GitHub branch"
                }))
            }
            Some(sha) if !sha.is_empty() => SuppliedBase::PushBefore(sha.to_string()),
            _ => SuppliedBase::Nothing(NoBase::new(format!("GitHub {} event", self.context.event))),
        }
    }

    /// The commit a merge group is built on. GitHub puts it in the event
    /// payload and in no environment variable.
    fn parse_merge_group_base_sha(event: &serde_json::Value) -> Option<String> {
        Self::commit_at(event, "/merge_group/base_sha")
    }

    fn parse_pr_base_sha(event: &serde_json::Value) -> Option<String> {
        Self::commit_at(event, "/pull_request/base/sha")
    }

    /// The commit an event payload names at a JSON pointer, unless it is
    /// missing, empty or the null commit.
    fn commit_at(event: &serde_json::Value, pointer: &str) -> Option<String> {
        event
            .pointer(pointer)?
            .as_str()
            .filter(|sha| !sha.is_empty() && *sha != NULL_SHA)
            .map(str::to_string)
    }

    /// Create an octocrab instance authenticated with the GitHub token.
    fn octocrab(&self) -> Result<Octocrab> {
        if self.token.is_empty() {
            return Err(cuenv_core::Error::configuration(
                "GITHUB_TOKEN is not set or empty",
            ));
        }
        Octocrab::builder()
            .personal_token(self.token.clone())
            .build()
            .map_err(|e| {
                cuenv_core::Error::configuration(format!("Failed to create GitHub client: {e}"))
            })
    }

    /// Get changed files for a PR using the GitHub API.
    ///
    /// This is a fallback for checkouts where the local merge diff is not
    /// available. The API returns paginated results, so every page must be
    /// followed or large PRs will be silently truncated. It lists no more
    /// than [`PULL_REQUEST_FILES_API`]'s limit over all of its pages, so a
    /// listing that long is refused.
    async fn get_pr_files_from_api(&self, pr_number: u64) -> Result<Vec<PathBuf>> {
        debug!("Fetching PR files from GitHub API for PR #{pr_number}");
        let octocrab = self.octocrab()?;

        let mut page = octocrab
            .pulls(&self.owner, &self.repo)
            .list_files(pr_number)
            .await
            .map_err(|e| {
                cuenv_core::Error::configuration(format!("Failed to get PR files from API: {e}"))
            })?;

        let mut entries = Vec::new();
        loop {
            entries.append(&mut page.items);

            let next_page = octocrab
                .get_page::<DiffEntry>(&page.next)
                .await
                .map_err(|e| {
                    cuenv_core::Error::configuration(format!(
                        "Failed to get next PR files page from API: {e}"
                    ))
                })?;

            let Some(next_page) = next_page else {
                break;
            };
            page = next_page;
        }
        let files: Vec<PathBuf> = PULL_REQUEST_FILES_API
            .complete(entries)?
            .iter()
            .flat_map(listed_paths)
            .collect();

        info!("Got {} changed files from GitHub API", files.len());
        Ok(files)
    }

    /// Get the files changed since a commit using the GitHub Compare API.
    ///
    /// Uses `GET /repos/{owner}/{repo}/compare/{base}...{head}` to determine
    /// which files changed between a commit GitHub named and this run's.
    /// This works regardless of clone depth and handles multi-commit pushes.
    async fn get_compare_files_from_api(&self, base_sha: &str) -> Result<Vec<PathBuf>> {
        debug!(
            "Fetching changed files from GitHub Compare API: {base_sha}...{}",
            &self.context.sha
        );
        let octocrab = self.octocrab()?;

        let comparison = octocrab
            .commits(&self.owner, &self.repo)
            .compare(base_sha, &self.context.sha)
            .send()
            .await
            .map_err(|e| {
                cuenv_core::Error::configuration(format!(
                    "Failed to get changed files from Compare API: {e}"
                ))
            })?;

        let comparison_files = comparison.files.ok_or_else(|| {
            cuenv_core::Error::configuration(
                "GitHub Compare API response missing 'files' field".to_string(),
            )
        })?;
        let files: Vec<PathBuf> = COMPARE_API
            .complete(comparison_files)?
            .iter()
            .flat_map(listed_paths)
            .collect();

        info!("Got {} changed files from GitHub Compare API", files.len());
        Ok(files)
    }

    /// Compare a pull request with its base.
    ///
    /// The checkout is compared only when it holds the pull request. When it
    /// holds something else, only GitHub can say what the pull request
    /// changes.
    async fn pull_request_changes(&self, event: &serde_json::Value) -> ChangedFiles {
        let holds_pull_request = self.checkout_holds_pull_request(event);

        if holds_pull_request && let Some(files) = self.pull_request_changes_in_checkout(event) {
            return ChangedFiles::Compared(files);
        }

        // Strategy 3: the paginated API listing, which is the same whatever
        // is checked out.
        if let Some(pr_number) = self.pull_request_number(event) {
            debug!("PR #{pr_number} detected, using GitHub API fallback for changed files");
            match self.get_pr_files_from_api(pr_number).await {
                Ok(files) => return ChangedFiles::Compared(files),
                Err(e) => warn!("Failed to get PR files from API: {e}"),
            }
        }

        if holds_pull_request && let Some(files) = self.changes_since_base_branch() {
            return ChangedFiles::Compared(files);
        }

        ChangedFiles::NoBase(NoBase::new(if holds_pull_request {
            "GitHub pull request whose base cannot be compared"
        } else {
            "GitHub pull request that is not checked out and whose files could not be listed"
        }))
    }

    /// What a pull request changes, read from a checkout that holds it;
    /// `None` when the checkout does not have the history to say.
    fn pull_request_changes_in_checkout(&self, event: &serde_json::Value) -> Option<Vec<PathBuf>> {
        // Strategy 1: the checkout's synthetic merge commit. GitHub's
        // default pull_request checkout is refs/pull/<number>/merge; HEAD^1
        // is the base and HEAD includes the complete PR result, no matter
        // how many commits the PR contains. That parent is the base GitHub
        // merged the pull request into, not a guess.
        if self.is_pr_merge_ref()
            && self.is_synthetic_merge_checkout()
            && self.checkout.head_is_merge()
            && let Some(files) = self.checkout.changed(Since::Revision("HEAD^1"))
        {
            info!(
                "Got {} changed files from the pull request merge diff",
                files.len()
            );
            return Some(files);
        }

        // Strategy 2: a workflow may explicitly check out the PR head
        // instead of the merge ref. Use the event's immutable base SHA in
        // that case so a moving base branch cannot change the affected-task
        // result.
        let base_sha = Self::parse_pr_base_sha(event)?;
        debug!("Pull request detected, using base SHA {base_sha}");
        if !self.checkout.has_commit(&base_sha) {
            self.checkout.fetch(Fetch::Commit(&base_sha));
        }
        let files = self.checkout.changed(Since::Revision(&base_sha))?;
        info!(
            "Got {} changed files from the pull request base SHA diff",
            files.len()
        );
        Some(files)
    }

    /// Strategy 4: what a checkout that holds a pull request has that the
    /// tip of its base branch does not, as a last resort.
    fn changes_since_base_branch(&self) -> Option<Vec<PathBuf>> {
        let base = self
            .context
            .base_ref
            .as_deref()
            .filter(|base| !base.is_empty())?;
        debug!("PR detected, base_ref: {base}");

        if self.checkout.is_shallow() {
            self.checkout.fetch(Fetch::Branch(base));
        }

        self.checkout
            .changed(Since::MergeBaseWith(&format!("origin/{base}")))
    }

    /// Compare the run with a commit GitHub named for it. `run` says what
    /// that commit is to the run, for a run it gives no base after all.
    async fn compared_with(&self, sha: &str, run: &str) -> ChangedFiles {
        match self.changes_since(sha).await {
            Some(files) => ChangedFiles::Compared(files),
            None => ChangedFiles::NoBase(NoBase::new(format!(
                "{run} {} cannot be compared",
                short_sha(sha)
            ))),
        }
    }

    /// The files that differ between a commit and this run; `None` when the
    /// two cannot be compared.
    async fn changes_since(&self, sha: &str) -> Option<Vec<PathBuf>> {
        // In the checkout, when it holds the commit.
        if let Some(files) = self.checkout.changed(Since::Revision(sha)) {
            info!(
                "Got {} changed files by comparing {sha} with HEAD",
                files.len()
            );
            return Some(files);
        }

        // Through the Compare API, which needs no history: a default
        // checkout holds one commit, and two with `fetch-depth: 2`.
        match self.get_compare_files_from_api(sha).await {
            Ok(files) => return Some(files),
            Err(e) => warn!("Failed to compare {sha} with this run through the GitHub API: {e}"),
        }

        // By fetching the commit into a shallow checkout.
        if self.checkout.is_shallow() && self.checkout.fetch(Fetch::Commit(sha)) {
            return self.checkout.changed(Since::Revision(sha));
        }
        None
    }
}

#[async_trait]
impl CIProvider for GitHubCIProvider {
    fn detect() -> Option<Self> {
        if std::env::var("GITHUB_ACTIONS").ok()? != "true" {
            return None;
        }

        let repo_str = std::env::var("GITHUB_REPOSITORY").ok()?;
        let (owner, repo) = Self::parse_repo(&repo_str);

        let github_ref = std::env::var("GITHUB_REF").unwrap_or_default();
        let pr_number = Self::parse_pr_number(&github_ref);
        let token = std::env::var("GITHUB_TOKEN").unwrap_or_default();
        let checkout = RunCheckout::with_token(&token);

        Some(Self {
            context: CIContext {
                provider: "github".to_string(),
                event: std::env::var("GITHUB_EVENT_NAME").unwrap_or_default(),
                ref_name: std::env::var("GITHUB_REF_NAME").unwrap_or_default(),
                base_ref: std::env::var("GITHUB_BASE_REF").ok(),
                sha: std::env::var("GITHUB_SHA").unwrap_or_default(),
            },
            token,
            owner,
            repo,
            git_ref: github_ref,
            pr_number,
            checkout,
        })
    }

    fn context(&self) -> &CIContext {
        &self.context
    }

    async fn changed_files(&self) -> Result<ChangedFiles> {
        let event = Self::event_payload();
        let explicit_before = std::env::var("GITHUB_BEFORE").ok();

        Ok(
            match self.supplied_base(&event, explicit_before.as_deref()) {
                SuppliedBase::PullRequest => self.pull_request_changes(&event).await,
                SuppliedBase::PushBefore(sha) => {
                    self.compared_with(&sha, "GitHub push whose previous commit")
                        .await
                }
                SuppliedBase::MergeGroupBase(sha) => {
                    self.compared_with(&sha, "GitHub merge queue run whose base")
                        .await
                }
                SuppliedBase::Nothing(why) => ChangedFiles::NoBase(why),
            },
        )
    }

    async fn create_check(&self, name: &str) -> Result<CheckHandle> {
        let octocrab = self.octocrab()?;

        let check_run = octocrab
            .checks(&self.owner, &self.repo)
            .create_check_run(name, &self.context.sha)
            .status(octocrab::params::checks::CheckRunStatus::InProgress)
            .send()
            .await
            .map_err(|e| {
                cuenv_core::Error::configuration(format!("Failed to create check run: {e}"))
            })?;

        info!("Created check run: {} (id: {})", name, check_run.id);

        Ok(CheckHandle {
            id: check_run.id.to_string(),
        })
    }

    async fn update_check(&self, handle: &CheckHandle, summary: &str) -> Result<()> {
        let octocrab = self.octocrab()?;
        let check_run_id: u64 = handle
            .id
            .parse()
            .map_err(|_| cuenv_core::Error::configuration("Invalid check run ID"))?;

        octocrab
            .checks(&self.owner, &self.repo)
            .update_check_run(check_run_id.into())
            .output(octocrab::params::checks::CheckRunOutput {
                title: "cuenv CI".to_string(),
                summary: summary.to_string(),
                text: None,
                annotations: vec![],
                images: vec![],
            })
            .send()
            .await
            .map_err(|e| {
                cuenv_core::Error::configuration(format!("Failed to update check run: {e}"))
            })?;

        Ok(())
    }

    async fn complete_check(&self, handle: &CheckHandle, report: &PipelineReport) -> Result<()> {
        let octocrab = self.octocrab()?;
        let check_run_id: u64 = handle
            .id
            .parse()
            .map_err(|_| cuenv_core::Error::configuration("Invalid check run ID"))?;

        let conclusion = match report.status {
            PipelineStatus::Success => octocrab::params::checks::CheckRunConclusion::Success,
            PipelineStatus::Failed => octocrab::params::checks::CheckRunConclusion::Failure,
            PipelineStatus::Partial | PipelineStatus::Pending => {
                octocrab::params::checks::CheckRunConclusion::Neutral
            }
        };

        let summary = markdown::generate_summary(report);

        octocrab
            .checks(&self.owner, &self.repo)
            .update_check_run(check_run_id.into())
            .status(octocrab::params::checks::CheckRunStatus::Completed)
            .conclusion(conclusion)
            .output(octocrab::params::checks::CheckRunOutput {
                title: format!("cuenv: {}", report.project),
                summary,
                text: None,
                annotations: vec![],
                images: vec![],
            })
            .send()
            .await
            .map_err(|e| {
                cuenv_core::Error::configuration(format!("Failed to complete check run: {e}"))
            })?;

        info!("Completed check run: {}", handle.id);

        Ok(())
    }

    async fn upload_report(&self, report: &PipelineReport) -> Result<Option<String>> {
        // Only post PR comments for pull_request events
        let Some(pr_number) = self.pr_number else {
            debug!("Not a PR event, skipping PR comment");
            return Ok(None);
        };

        let octocrab = self.octocrab()?;
        let summary = markdown::generate_summary(report);

        let comment = octocrab
            .issues(&self.owner, &self.repo)
            .create_comment(pr_number, &summary)
            .await
            .map_err(|e| {
                cuenv_core::Error::configuration(format!("Failed to post PR comment: {e}"))
            })?;

        info!("Posted PR comment: {}", comment.html_url);

        Ok(Some(comment.html_url.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_repo() {
        let (owner, repo) = GitHubCIProvider::parse_repo("cuenv/cuenv");
        assert_eq!(owner, "cuenv");
        assert_eq!(repo, "cuenv");
    }

    #[test]
    fn test_parse_repo_different_names() {
        let (owner, repo) = GitHubCIProvider::parse_repo("organization/project-name");
        assert_eq!(owner, "organization");
        assert_eq!(repo, "project-name");
    }

    #[test]
    fn test_parse_repo_invalid() {
        let (owner, repo) = GitHubCIProvider::parse_repo("invalid");
        assert_eq!(owner, "");
        assert_eq!(repo, "");
    }

    #[test]
    fn test_parse_repo_empty() {
        let (owner, repo) = GitHubCIProvider::parse_repo("");
        assert_eq!(owner, "");
        assert_eq!(repo, "");
    }

    #[test]
    fn test_parse_repo_too_many_parts() {
        let (owner, repo) = GitHubCIProvider::parse_repo("a/b/c/d");
        assert_eq!(owner, "");
        assert_eq!(repo, "");
    }

    #[test]
    fn test_null_sha_constant() {
        assert_eq!(NULL_SHA.len(), 40);
        assert!(NULL_SHA.chars().all(|c| c == '0'));
    }

    #[test]
    fn test_parse_pr_number() {
        assert_eq!(
            GitHubCIProvider::parse_pr_number("refs/pull/123/merge"),
            Some(123)
        );
        assert_eq!(
            GitHubCIProvider::parse_pr_number("refs/pull/456/head"),
            Some(456)
        );
        assert_eq!(GitHubCIProvider::parse_pr_number("refs/heads/main"), None);
        assert_eq!(GitHubCIProvider::parse_pr_number("main"), None);
    }

    #[test]
    fn test_parse_pr_number_large() {
        assert_eq!(
            GitHubCIProvider::parse_pr_number("refs/pull/999999/merge"),
            Some(999_999)
        );
    }

    #[test]
    fn test_parse_pr_number_zero() {
        // Edge case: PR number 0 (unlikely but possible in parsing)
        assert_eq!(
            GitHubCIProvider::parse_pr_number("refs/pull/0/merge"),
            Some(0)
        );
    }

    #[test]
    fn test_parse_pr_number_empty() {
        assert_eq!(GitHubCIProvider::parse_pr_number(""), None);
    }

    #[test]
    fn test_parse_pr_number_refs_pull_only() {
        assert_eq!(GitHubCIProvider::parse_pr_number("refs/pull/"), None);
    }

    #[test]
    fn test_parse_pr_number_invalid_number() {
        assert_eq!(
            GitHubCIProvider::parse_pr_number("refs/pull/abc/merge"),
            None
        );
    }

    #[test]
    fn test_parse_pr_number_branch_ref() {
        // Typical branch refs should return None
        assert_eq!(
            GitHubCIProvider::parse_pr_number("refs/heads/feature/test"),
            None
        );
        assert_eq!(
            GitHubCIProvider::parse_pr_number("refs/heads/develop"),
            None
        );
        assert_eq!(GitHubCIProvider::parse_pr_number("refs/tags/v1.0.0"), None);
    }

    const BEFORE: &str = "1111111111111111111111111111111111111111";
    const BASE: &str = "2222222222222222222222222222222222222222";

    /// A provider for a run of `event` on `git_ref`, as `detect` builds it.
    fn provider(event: &str, git_ref: &str) -> GitHubCIProvider {
        GitHubCIProvider {
            context: CIContext {
                provider: "github".to_string(),
                event: event.to_string(),
                ref_name: git_ref.rsplit('/').next().unwrap_or_default().to_string(),
                base_ref: Some(String::new()),
                sha: "3333333333333333333333333333333333333333".to_string(),
            },
            token: String::new(),
            owner: "example".to_string(),
            repo: "fixture".to_string(),
            git_ref: git_ref.to_string(),
            pr_number: GitHubCIProvider::parse_pr_number(git_ref),
            checkout: RunCheckout::default(),
        }
    }

    fn nothing(reason: &str) -> SuppliedBase {
        SuppliedBase::Nothing(NoBase::new(reason))
    }

    #[test]
    fn events_that_name_no_previous_commit_have_no_base() {
        let payloads = [
            ("schedule", serde_json::json!({"schedule": "0 4 * * *"})),
            ("workflow_dispatch", serde_json::json!({"inputs": {}})),
            ("release", serde_json::json!({"action": "published"})),
            (
                "deployment",
                serde_json::json!({"deployment": {"sha": BASE}}),
            ),
        ];
        for (event, payload) in payloads {
            assert_eq!(
                provider(event, "refs/heads/main").supplied_base(&payload, None),
                nothing(&format!("GitHub {event} event")),
            );
        }
    }

    #[test]
    fn a_tag_push_has_no_base() {
        let payload = serde_json::json!({"before": NULL_SHA, "ref": "refs/tags/1.2.3"});
        assert_eq!(
            provider("push", "refs/tags/1.2.3").supplied_base(&payload, None),
            nothing("GitHub tag push"),
        );
    }

    #[test]
    fn the_first_push_of_a_branch_has_no_base() {
        let payload = serde_json::json!({"before": NULL_SHA, "created": true});
        assert_eq!(
            provider("push", "refs/heads/feature").supplied_base(&payload, None),
            nothing("first push of a GitHub branch"),
        );
    }

    #[test]
    fn a_push_is_compared_with_the_commit_before_it() {
        let payload = serde_json::json!({"before": BEFORE});
        assert_eq!(
            provider("push", "refs/heads/main").supplied_base(&payload, None),
            SuppliedBase::PushBefore(BEFORE.to_string()),
        );
    }

    #[test]
    fn a_push_whose_payload_cannot_be_read_has_no_base() {
        assert_eq!(
            provider("push", "refs/heads/main").supplied_base(&serde_json::Value::Null, None),
            nothing("GitHub push event"),
        );
    }

    #[test]
    fn a_merge_queue_run_is_compared_with_the_base_of_its_merge_group() {
        let payload = serde_json::json!({
            "merge_group": {"base_sha": BASE, "head_sha": "head", "base_ref": "refs/heads/main"}
        });
        let queue = "refs/heads/gh-readonly-queue/main/pr-7-abc";
        assert_eq!(
            provider("merge_group", queue).supplied_base(&payload, None),
            SuppliedBase::MergeGroupBase(BASE.to_string()),
        );

        let without_base = serde_json::json!({"merge_group": {"base_sha": NULL_SHA}});
        assert_eq!(
            provider("merge_group", queue).supplied_base(&without_base, None),
            nothing("GitHub merge_group event"),
        );
    }

    #[test]
    fn a_pull_request_is_compared_with_its_base() {
        // A synchronize payload also carries the head `before` the push:
        // that is not the pull request's base.
        let payload = serde_json::json!({
            "before": BEFORE,
            "pull_request": {"base": {"sha": BASE}}
        });
        assert_eq!(
            provider("pull_request", "refs/pull/7/merge").supplied_base(&payload, None),
            SuppliedBase::PullRequest,
        );

        // `pull_request_target` runs on the base branch's ref and names the
        // branch the pull request targets.
        let mut targeting = provider("pull_request_target", "refs/heads/main");
        targeting.context.base_ref = Some("main".to_string());
        assert_eq!(
            targeting.supplied_base(&payload, None),
            SuppliedBase::PullRequest,
        );
    }

    #[test]
    fn github_before_stands_in_for_the_payload() {
        let payload = serde_json::json!({"before": BEFORE});
        let run = provider("push", "refs/heads/main");

        assert_eq!(
            run.supplied_base(&payload, Some(BASE)),
            SuppliedBase::PushBefore(BASE.to_string()),
        );
        assert_eq!(
            run.supplied_base(&payload, Some(NULL_SHA)),
            nothing("first push of a GitHub branch"),
        );
        assert_eq!(
            run.supplied_base(&payload, Some("")),
            nothing("GitHub push event"),
        );
    }

    #[test]
    fn a_listing_that_may_be_cut_short_is_not_a_comparison() {
        let listing = |count: usize| -> Vec<usize> { (0..count).collect() };

        for api in [COMPARE_API, PULL_REQUEST_FILES_API] {
            assert_eq!(
                api.complete(listing(api.limit - 1)).unwrap().len(),
                api.limit - 1
            );
            let error = api.complete(listing(api.limit)).unwrap_err().to_string();
            assert!(error.contains(api.api), "{error}");
            assert!(error.contains("may be incomplete"), "{error}");
        }
        assert_eq!(COMPARE_API.limit, 300);
        assert_eq!(PULL_REQUEST_FILES_API.limit, 3000);
    }

    #[test]
    fn a_pull_request_is_numbered_by_its_ref_or_by_its_event() {
        let payload = serde_json::json!({"pull_request": {"number": 12}});

        // `pull_request` runs on the pull request's own ref.
        assert_eq!(
            provider("pull_request", "refs/pull/7/merge").pull_request_number(&payload),
            Some(7)
        );
        // `pull_request_target` runs on the base branch's ref.
        let targeting = provider("pull_request_target", "refs/heads/main");
        assert_eq!(targeting.pull_request_number(&payload), Some(12));
        assert_eq!(
            targeting.pull_request_number(&serde_json::Value::Null),
            None
        );
    }

    #[test]
    fn only_a_pull_request_ref_is_a_merge_ref() {
        assert!(provider("pull_request", "refs/pull/7/merge").is_pr_merge_ref());
        assert!(!provider("pull_request", "refs/pull/7/head").is_pr_merge_ref());
        // A base branch may be named like one.
        assert!(!provider("pull_request_target", "refs/heads/feature/merge").is_pr_merge_ref());
    }

    #[test]
    fn a_checkout_of_something_else_does_not_hold_the_pull_request() {
        // Whatever this test runs in, it is not a checkout of these commits.
        let payload = serde_json::json!({
            "pull_request": {"number": 12, "head": {"sha": BEFORE}, "base": {"sha": BASE}}
        });

        let targeting = provider("pull_request_target", "refs/heads/main");
        assert!(!targeting.checkout_holds_pull_request(&payload));
        assert!(!targeting.checkout_holds_pull_request(&serde_json::Value::Null));
        // The merge ref counts only when its commit is what is checked out.
        assert!(
            !provider("pull_request", "refs/pull/7/merge").checkout_holds_pull_request(&payload)
        );
    }

    #[test]
    fn a_renamed_file_is_listed_under_both_of_its_names() {
        let entry = |extra: serde_json::Value| -> DiffEntry {
            let mut entry = serde_json::json!({
                "sha": BASE,
                "filename": "app/new.txt",
                "status": "modified",
                "additions": 1,
                "deletions": 1,
                "changes": 2,
                "blob_url": null,
                "raw_url": null,
                "contents_url": "https://api.github.com/repos/example/fixture/contents/app/new.txt",
            });
            for (key, value) in extra.as_object().into_iter().flatten() {
                entry[key] = value.clone();
            }
            serde_json::from_value(entry).unwrap()
        };

        let modified = entry(serde_json::json!({}));
        let renamed = entry(serde_json::json!({
            "status": "renamed",
            "previous_filename": "content/old.txt",
        }));

        assert_eq!(
            listed_paths(&modified).collect::<Vec<_>>(),
            [PathBuf::from("app/new.txt")]
        );
        assert_eq!(
            listed_paths(&renamed).collect::<Vec<_>>(),
            [
                PathBuf::from("content/old.txt"),
                PathBuf::from("app/new.txt")
            ]
        );
    }

    #[test]
    fn a_short_sha_is_the_first_seven_characters() {
        assert_eq!(short_sha(BEFORE), "1111111");
        assert_eq!(short_sha("abc"), "abc");
    }

    #[test]
    fn the_event_payload_is_read_from_the_path_github_gives() {
        let dir = tempfile::tempdir().unwrap();
        let event_path = dir.path().join("event.json");
        std::fs::write(&event_path, r#"{"before": "abc123def456"}"#).unwrap();

        temp_env::with_var(
            "GITHUB_EVENT_PATH",
            Some(event_path.to_str().unwrap()),
            || {
                assert_eq!(GitHubCIProvider::event_payload()["before"], "abc123def456");
            },
        );
        temp_env::with_var_unset("GITHUB_EVENT_PATH", || {
            assert!(GitHubCIProvider::event_payload().is_null());
        });
    }

    #[test]
    fn test_parse_merge_group_base_sha() {
        let event = serde_json::json!({"merge_group": {"base_sha": BASE}});
        assert_eq!(
            GitHubCIProvider::parse_merge_group_base_sha(&event),
            Some(BASE.to_string())
        );
        assert_eq!(
            GitHubCIProvider::parse_merge_group_base_sha(&serde_json::json!({"before": BEFORE})),
            None
        );
    }

    #[test]
    fn test_parse_pr_base_sha() {
        let event = serde_json::json!({
            "pull_request": {
                "base": {
                    "sha": "abc123def456"
                }
            }
        });

        assert_eq!(
            GitHubCIProvider::parse_pr_base_sha(&event),
            Some("abc123def456".to_string())
        );
    }

    #[test]
    fn test_parse_pr_base_sha_rejects_missing_or_null_sha() {
        let missing = serde_json::json!({"pull_request": {"base": {}}});
        assert_eq!(GitHubCIProvider::parse_pr_base_sha(&missing), None);

        let null_sha = serde_json::json!({
            "pull_request": {
                "base": {
                    "sha": NULL_SHA
                }
            }
        });
        assert_eq!(GitHubCIProvider::parse_pr_base_sha(&null_sha), None);
    }

    #[test]
    fn test_detect_not_github_actions() {
        // Clear GitHub Actions environment variables
        temp_env::with_vars_unset(["GITHUB_ACTIONS", "GITHUB_REPOSITORY"], || {
            let provider = GitHubCIProvider::detect();
            assert!(provider.is_none());
        });
    }

    #[test]
    fn test_detect_github_actions_false() {
        temp_env::with_var("GITHUB_ACTIONS", Some("false"), || {
            let provider = GitHubCIProvider::detect();
            assert!(provider.is_none());
        });
    }
}
