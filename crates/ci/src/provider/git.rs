//! What a CI provider reads from the git checkout it runs in.
//!
//! The repository is read in process with `gix`: cuenv does not run `git` to
//! learn about a checkout. Paths are relative to the root of the worktree.

use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use gix::bstr::BStr;
use gix::credentials::helper::Action;
use gix::remote::Direction;
use gix::remote::fetch::{Shallow, Tags};

/// Why a fetch did not happen.
type FetchError = Box<dyn std::error::Error + Send + Sync>;

/// What to fetch from `origin` into a shallow checkout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fetch<'a> {
    /// The tip of a branch, kept as `origin/<branch>`.
    Branch(&'a str),
    /// One commit, by its hash.
    Commit(&'a str),
    /// One more commit of history behind everything already fetched.
    OneMoreCommit,
}

/// Where a comparison with `HEAD` starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Since<'a> {
    /// The revision itself: `git diff <revision>..HEAD`.
    Revision(&'a str),
    /// The commit `HEAD` and the revision both descend from:
    /// `git diff <revision>...HEAD`.
    MergeBaseWith(&'a str),
}

/// The git checkout a CI provider runs in.
#[derive(Debug)]
pub struct Checkout {
    repository: gix::Repository,
    /// The token to offer `origin` when it asks who is fetching.
    token: Option<String>,
}

impl Checkout {
    /// The checkout around the working directory; `None` outside a
    /// repository.
    #[must_use]
    pub fn discover() -> Option<Self> {
        Self::around(Path::new("."))
    }

    /// The checkout around a directory; `None` outside a repository.
    #[must_use]
    pub fn around(directory: &Path) -> Option<Self> {
        let repository = gix::discover(directory).ok()?;
        Some(Self {
            repository,
            token: None,
        })
    }

    /// Fetch with an access token. CI checkouts keep their token where only
    /// `git` looks for it, so the provider hands over the one it was given.
    #[must_use]
    pub fn with_token(mut self, token: &str) -> Self {
        self.token = (!token.is_empty()).then(|| token.to_string());
        self
    }

    /// Whether history was cut off when the repository was cloned.
    #[must_use]
    pub fn is_shallow(&self) -> bool {
        self.repository.is_shallow()
    }

    /// Whether a revision names a commit this checkout has.
    #[must_use]
    pub fn has_commit(&self, revision: &str) -> bool {
        self.commit(revision).is_some()
    }

    /// Whether `HEAD` is a merge commit.
    #[must_use]
    pub fn head_is_merge(&self) -> bool {
        self.repository
            .head_commit()
            .is_ok_and(|head| head.parent_ids().count() > 1)
    }

    /// Whether `HEAD` is the commit with this hash.
    #[must_use]
    pub fn head_is(&self, sha: &str) -> bool {
        self.repository
            .head_id()
            .is_ok_and(|head| head.to_string().eq_ignore_ascii_case(sha))
    }

    /// The files that differ between a starting point and `HEAD`; `None` when
    /// either side is missing, as it is past the edge of a shallow clone.
    ///
    /// A renamed file is reported under both names, because both the project
    /// it left and the one it joined have changed.
    #[must_use]
    pub fn changed(&self, since: Since<'_>) -> Option<Vec<PathBuf>> {
        let head = self.repository.head_commit().ok()?;
        let from = match since {
            Since::Revision(revision) => self.commit(revision)?,
            Since::MergeBaseWith(revision) => {
                let other = self.commit(revision)?;
                let base = self.repository.merge_base(other.id, head.id).ok()?;
                base.object().ok()?.try_into_commit().ok()?
            }
        };
        self.changed_between(&from.tree().ok()?, &head.tree().ok()?)
    }

    /// Fetch from `origin` over the remote's own transport. cuenv runs no
    /// `git`; an SSH remote is reached through the `ssh` program, as git
    /// reaches it.
    ///
    /// # Errors
    ///
    /// Fails when there is no `origin`, it cannot be reached, or it does not
    /// have what was asked for.
    pub fn fetch(&self, fetch: Fetch<'_>) -> Result<(), FetchError> {
        let mut origin = self.repository.find_remote("origin")?;
        let depth = match fetch {
            Fetch::Branch(branch) => {
                let refspec = format!("+refs/heads/{branch}:refs/remotes/origin/{branch}");
                origin.replace_refspecs(Some(refspec.as_str()), Direction::Fetch)?;
                Shallow::DepthAtRemote(NonZeroU32::MIN)
            }
            Fetch::Commit(sha) => {
                origin.replace_refspecs(Some(sha), Direction::Fetch)?;
                Shallow::DepthAtRemote(NonZeroU32::MIN)
            }
            Fetch::OneMoreCommit => Shallow::Deepen(1),
        };
        let origin = origin.with_fetch_tags(Tags::None);
        let connection = origin.connect(Direction::Fetch)?;
        #[expect(
            clippy::result_large_err,
            reason = "gix decides what a credentials callback returns"
        )]
        let connection = match self.token.as_deref() {
            Some(token) => {
                connection.with_credentials(move |action| Ok(token_identity(action, token)))
            }
            None => connection,
        };
        connection
            .prepare_fetch(
                gix::progress::Discard,
                gix::remote::ref_map::Options::default(),
            )?
            .with_shallow(depth)
            .receive(gix::progress::Discard, &AtomicBool::new(false))?;
        Ok(())
    }

    /// The tracked files that differ from `HEAD` in the index or the
    /// worktree: `git diff HEAD`.
    #[must_use]
    pub fn uncommitted(&self) -> Option<Vec<PathBuf>> {
        let changes = self
            .repository
            .status(gix::progress::Discard)
            .ok()?
            .untracked_files(gix::status::UntrackedFiles::None)
            .into_iter(None)
            .ok()?;
        let paths: BTreeSet<PathBuf> = changes
            .filter_map(std::result::Result::ok)
            .map(|change| path(change.location()))
            .collect();
        Some(paths.into_iter().collect())
    }

    /// Every file the index tracks: `git ls-files` from the root.
    #[must_use]
    pub fn tracked_files(&self) -> Vec<PathBuf> {
        self.repository
            .index_or_empty()
            .map(|index| {
                let paths: BTreeSet<PathBuf> = index
                    .entries()
                    .iter()
                    .map(|entry| path(entry.path(&index)))
                    .collect();
                paths.into_iter().collect()
            })
            .unwrap_or_default()
    }

    /// The commit a revision names.
    fn commit(&self, revision: &str) -> Option<gix::Commit<'_>> {
        let id = self.repository.rev_parse_single(revision).ok()?;
        id.object().ok()?.peel_to_commit().ok()
    }

    /// The files that differ between two trees.
    fn changed_between(&self, from: &gix::Tree<'_>, to: &gix::Tree<'_>) -> Option<Vec<PathBuf>> {
        let options = gix::diff::Options::default().with_rewrites(None);
        let changes = self.repository.diff_tree_to_tree(from, to, options).ok()?;
        Some(
            changes
                .iter()
                .filter(|change| !change.entry_mode().is_tree())
                .map(|change| path(change.location()))
                .collect(),
        )
    }
}

/// Answer a remote that asks for credentials with an access token.
fn token_identity(action: Action, token: &str) -> Option<gix::credentials::protocol::Outcome> {
    match action {
        Action::Get(context) => Some(gix::credentials::protocol::Outcome {
            identity: gix::sec::identity::Account {
                username: "x-access-token".to_string(),
                password: token.to_string(),
                oauth_refresh_token: None,
            },
            next: context.into(),
        }),
        Action::Store(_) | Action::Erase(_) => None,
    }
}

/// A path of the repository as a path of the filesystem.
fn path(location: &BStr) -> PathBuf {
    gix::path::from_bstr(location).into_owned()
}

#[cfg(test)]
#[path = "git_tests.rs"]
mod tests;
