//! What a CI provider reads from the git checkout it runs in.
//!
//! The repository is read in process with `gix`: cuenv does not run `git` to
//! learn about a checkout. Paths are relative to the root of the worktree.

use std::collections::BTreeSet;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
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

/// Why the uncommitted changes of a checkout could not be listed.
pub type StatusError = Box<dyn std::error::Error + Send + Sync>;

/// The git checkout a CI provider runs in. It is discovered once and shared:
/// every question is answered from the same repository.
#[derive(Debug)]
pub struct Checkout {
    repository: gix::ThreadSafeRepository,
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
        let repository = gix::discover(directory).ok()?.into_sync();
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
        self.repository().is_shallow()
    }

    /// The repository, for the thread that asks.
    fn repository(&self) -> gix::Repository {
        self.repository.to_thread_local()
    }

    /// Whether a revision names a commit this checkout has.
    #[must_use]
    pub fn has_commit(&self, revision: &str) -> bool {
        commit(&self.repository(), revision).is_some()
    }

    /// Whether `HEAD` is a merge commit.
    #[must_use]
    pub fn head_is_merge(&self) -> bool {
        self.repository()
            .head_commit()
            .is_ok_and(|head| head.parent_ids().count() > 1)
    }

    /// Whether `HEAD` is the commit with this hash.
    #[must_use]
    pub fn head_is(&self, sha: &str) -> bool {
        self.repository()
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
        let repository = self.repository();
        let head = repository.head_commit().ok()?;
        let from = match since {
            Since::Revision(revision) => commit(&repository, revision)?,
            Since::MergeBaseWith(revision) => {
                let other = commit(&repository, revision)?;
                let base = repository.merge_base(other.id, head.id).ok()?;
                base.object().ok()?.try_into_commit().ok()?
            }
        };
        changed_between(&repository, &from.tree().ok()?, &head.tree().ok()?)
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
        let repository = self.repository();
        let mut origin = repository.find_remote("origin")?;
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
        let connection = match self.token.as_deref() {
            Some(token) => connection.with_credentials(answering_with(token)),
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
    ///
    /// A renamed file is reported under both names, as [`Self::changed`]
    /// reports it.
    ///
    /// # Errors
    ///
    /// Fails when the index or a tracked file cannot be read: a change that
    /// cannot be seen is not the absence of one.
    pub fn uncommitted(&self) -> Result<Vec<PathBuf>, StatusError> {
        let changes = self
            .repository()
            .status(gix::progress::Discard)?
            .untracked_files(gix::status::UntrackedFiles::None)
            .index_worktree_rewrites(None)
            .tree_index_track_renames(gix::status::tree_index::TrackRenames::Disabled)
            .into_iter(None)?;
        let paths = changes
            .map(|change| Ok(path(change?.location())))
            .collect::<Result<BTreeSet<PathBuf>, StatusError>>()?;
        Ok(paths.into_iter().collect())
    }

    /// Every file the index tracks: `git ls-files` from the root.
    #[must_use]
    pub fn tracked_files(&self) -> Vec<PathBuf> {
        self.repository()
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
}

/// The commit a revision names.
fn commit<'repository>(
    repository: &'repository gix::Repository,
    revision: &str,
) -> Option<gix::Commit<'repository>> {
    let id = repository.rev_parse_single(revision).ok()?;
    id.object().ok()?.peel_to_commit().ok()
}

/// The files that differ between two trees.
fn changed_between(
    repository: &gix::Repository,
    from: &gix::Tree<'_>,
    to: &gix::Tree<'_>,
) -> Option<Vec<PathBuf>> {
    let options = gix::diff::Options::default().with_rewrites(None);
    let changes = repository.diff_tree_to_tree(from, to, options).ok()?;
    Some(
        changes
            .iter()
            .filter(|change| !change.entry_mode().is_tree())
            .map(|change| path(change.location()))
            .collect(),
    )
}

/// A credentials callback that answers with an access token. It cannot
/// fail, so it does not choose the error a caller expects of it.
fn answering_with<Failure>(
    token: &str,
) -> impl FnMut(Action) -> Result<Option<gix::credentials::protocol::Outcome>, Failure> + '_ {
    move |action| Ok(token_identity(action, token))
}

/// The checkout a CI run is in, as a provider asks about it.
///
/// It is looked for the first time it is needed and kept, so every question
/// is answered from the same repository. Outside a repository nothing is
/// known: no commit exists, nothing changed and nothing is tracked.
#[derive(Debug, Default)]
pub struct RunCheckout {
    /// The token to offer `origin` when it asks who is fetching.
    token: Option<String>,
    checkout: OnceLock<Option<Checkout>>,
}

impl RunCheckout {
    /// The checkout of a run that fetches with an access token.
    #[must_use]
    pub fn with_token(token: &str) -> Self {
        Self {
            token: Some(token.to_string()),
            checkout: OnceLock::new(),
        }
    }

    fn checkout(&self) -> Option<&Checkout> {
        self.checkout
            .get_or_init(|| {
                let checkout = Checkout::discover()?;
                Some(match self.token.as_deref() {
                    Some(token) => checkout.with_token(token),
                    None => checkout,
                })
            })
            .as_ref()
    }

    /// Whether history was cut off when the repository was cloned.
    #[must_use]
    pub fn is_shallow(&self) -> bool {
        self.checkout().is_some_and(Checkout::is_shallow)
    }

    /// Fetch from `origin`, and say whether that worked.
    pub fn fetch(&self, fetch: Fetch<'_>) -> bool {
        tracing::debug!("Fetching {fetch:?}");
        self.checkout().is_some_and(|checkout| {
            checkout
                .fetch(fetch)
                .inspect_err(|error| tracing::debug!("Could not fetch {fetch:?}: {error}"))
                .is_ok()
        })
    }

    /// Whether a revision names a commit the checkout has.
    #[must_use]
    pub fn has_commit(&self, revision: &str) -> bool {
        self.checkout()
            .is_some_and(|checkout| checkout.has_commit(revision))
    }

    /// Whether `HEAD` is a merge commit.
    #[must_use]
    pub fn head_is_merge(&self) -> bool {
        self.checkout().is_some_and(Checkout::head_is_merge)
    }

    /// Whether `HEAD` is the commit with this hash.
    #[must_use]
    pub fn head_is(&self, sha: &str) -> bool {
        self.checkout()
            .is_some_and(|checkout| checkout.head_is(sha))
    }

    /// The files that differ between a starting point and `HEAD` (see
    /// [`Checkout::changed`]).
    #[must_use]
    pub fn changed(&self, since: Since<'_>) -> Option<Vec<PathBuf>> {
        tracing::debug!("Comparing HEAD with {since:?}");
        self.checkout()?.changed(since)
    }

    /// Every file the index tracks.
    #[must_use]
    pub fn tracked_files(&self) -> Vec<PathBuf> {
        self.checkout()
            .map(Checkout::tracked_files)
            .unwrap_or_default()
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
