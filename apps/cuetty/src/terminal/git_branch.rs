//! Which git branch a directory is on, read from the repository's `HEAD` file. No
//! process is spawned, and a tab reads the file only every so often, so naming the
//! branch costs a draw nothing.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How long a directory's branch is taken as read. A checkout elsewhere shows up within
/// this long; moving to another directory shows up at the next sample.
const RECHECK_INTERVAL: Duration = Duration::from_secs(2);

/// Hex digits of a detached commit worth showing, as `git log --oneline` does.
const SHORT_COMMIT: usize = 7;

/// More than a `HEAD` or `.git` file ever holds, which also keeps a device or a very
/// large file somewhere in a repository from being read at length.
const POINTER_FILE_LIMIT: u64 = 4096;

/// What a reftable repository's `HEAD` file always names. Its real branch is in the
/// reftable, which is not worth parsing for a label, so such a repository names none.
const REFTABLE_PLACEHOLDER: &str = ".invalid";

/// What a `HEAD` file says: a branch's name, or the short form of a detached commit.
fn branch_in_head(contents: &str) -> Option<String> {
    let contents = contents.trim();
    if let Some(reference) = contents.strip_prefix("ref:") {
        let reference = reference.trim();
        let name = reference
            .strip_prefix("refs/heads/")
            .or_else(|| reference.strip_prefix("refs/"))?;
        return (!name.is_empty() && name != REFTABLE_PLACEHOLDER).then(|| name.to_owned());
    }
    let is_commit =
        contents.len() >= SHORT_COMMIT && contents.chars().all(|digit| digit.is_ascii_hexdigit());
    is_commit.then(|| contents[..SHORT_COMMIT].to_owned())
}

/// The directory a worktree's or submodule's `.git` file points to.
fn gitdir_in_file(contents: &str, relative_to: &Path) -> Option<PathBuf> {
    let target = contents.trim().strip_prefix("gitdir:")?.trim();
    (!target.is_empty()).then(|| relative_to.join(target))
}

/// The contents of a small regular file. Anything else, a FIFO above all, is left
/// unread, because reading one could block the window.
fn read_pointer_file(path: &Path) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > POINTER_FILE_LIMIT {
        return None;
    }
    let mut contents = String::new();
    File::open(path)
        .ok()?
        .take(POINTER_FILE_LIMIT)
        .read_to_string(&mut contents)
        .ok()?;
    Some(contents)
}

/// The git directory of the nearest repository at or above `directory`. The nearest
/// `.git` of any kind settles it: a broken one says nothing about the branch, and
/// looking further up would report an enclosing repository's instead.
fn git_dir(directory: &Path) -> Option<PathBuf> {
    let ancestor = directory
        .ancestors()
        .find(|ancestor| std::fs::symlink_metadata(ancestor.join(".git")).is_ok())?;
    let candidate = ancestor.join(".git");
    if candidate.is_dir() {
        Some(candidate)
    } else {
        gitdir_in_file(&read_pointer_file(&candidate)?, ancestor)
    }
}

/// The branch `directory` is on, or `None` outside a repository.
fn current_branch(directory: &Path) -> Option<String> {
    let head = git_dir(directory)?.join("HEAD");
    branch_in_head(&read_pointer_file(&head)?)
}

/// A tab's record of the branch its directory is on, kept up to date by sampling.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct BranchProbe {
    directory: Option<PathBuf>,
    branch: Option<String>,
    checked_at: Option<Instant>,
}

impl BranchProbe {
    pub(super) fn branch(&self) -> Option<&str> {
        self.branch.as_deref()
    }

    fn is_due(&self, directory: Option<&Path>, now: Instant) -> bool {
        self.directory.as_deref() != directory
            || self
                .checked_at
                .is_none_or(|checked| now.duration_since(checked) >= RECHECK_INTERVAL)
    }

    /// Looks again if the directory moved or the last look has gone stale.
    pub(super) fn sample(&mut self, directory: Option<&Path>, now: Instant) {
        if self.is_due(directory, now) {
            self.directory = directory.map(Path::to_path_buf);
            self.branch = directory.and_then(current_branch);
            self.checked_at = Some(now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// An empty directory that is gone when the test is.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicU32 = AtomicU32::new(0);
            let path = std::env::temp_dir().join(format!(
                "cuetty-git-branch-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn write(&self, relative: &str, contents: &str) -> PathBuf {
            let path = self.0.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, contents).unwrap();
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn head_names_a_branch_whatever_its_slashes() {
        assert_eq!(
            branch_in_head("ref: refs/heads/main\n").as_deref(),
            Some("main")
        );
        assert_eq!(
            branch_in_head("ref: refs/heads/feature/rail\n").as_deref(),
            Some("feature/rail")
        );
    }

    #[test]
    fn a_detached_head_shows_its_commit_in_short() {
        assert_eq!(
            branch_in_head("550b9ab58ce4aa2bb088c9ea5dbd850e270882e8\n").as_deref(),
            Some("550b9ab")
        );
    }

    #[test]
    fn a_head_that_says_nothing_recognisable_names_nothing() {
        for head in [
            "",
            "\n",
            "ref:",
            "ref: refs/heads/",
            "ref: refs/heads/.invalid\n",
            "not a commit",
            "abc",
        ] {
            assert_eq!(branch_in_head(head), None, "{head:?}");
        }
    }

    #[test]
    fn a_directory_inside_a_repository_is_on_its_branch() {
        let scratch = Scratch::new();
        scratch.write(".git/HEAD", "ref: refs/heads/main\n");
        let nested = scratch.0.join("crates/core");
        fs::create_dir_all(&nested).unwrap();
        assert_eq!(current_branch(&nested).as_deref(), Some("main"));
    }

    #[test]
    fn a_worktree_follows_its_git_file_to_its_own_head() {
        let scratch = Scratch::new();
        scratch.write("main/.git/worktrees/rail/HEAD", "ref: refs/heads/rail\n");
        scratch.write("main/.git/HEAD", "ref: refs/heads/main\n");
        scratch.write("rail/.git", "gitdir: ../main/.git/worktrees/rail\n");
        assert_eq!(
            current_branch(&scratch.0.join("rail")).as_deref(),
            Some("rail")
        );
    }

    #[test]
    fn a_broken_repository_names_no_branch_not_even_an_enclosing_ones() {
        let scratch = Scratch::new();
        scratch.write(".git/HEAD", "ref: refs/heads/main\n");
        scratch.write("no-head/.git/config", "");
        assert_eq!(current_branch(&scratch.0.join("no-head")), None);
        scratch.write("bad-pointer/.git", "not a pointer\n");
        assert_eq!(current_branch(&scratch.0.join("bad-pointer")), None);
    }

    #[test]
    fn an_oversized_head_is_left_unread() {
        let scratch = Scratch::new();
        let oversized = "a".repeat(POINTER_FILE_LIMIT as usize + 1);
        scratch.write(".git/HEAD", &oversized);
        assert_eq!(current_branch(&scratch.0), None);
    }

    #[test]
    fn the_probe_looks_again_only_when_due() {
        let scratch = Scratch::new();
        let head = scratch.write(".git/HEAD", "ref: refs/heads/main\n");
        let start = Instant::now();
        let mut probe = BranchProbe::default();
        probe.sample(Some(&scratch.0), start);
        assert_eq!(probe.branch(), Some("main"));

        fs::write(&head, "ref: refs/heads/rail\n").unwrap();
        probe.sample(Some(&scratch.0), start + RECHECK_INTERVAL / 2);
        assert_eq!(probe.branch(), Some("main"));
        probe.sample(Some(&scratch.0), start + RECHECK_INTERVAL);
        assert_eq!(probe.branch(), Some("rail"));
    }

    #[test]
    fn the_probe_follows_a_change_of_directory_at_once() {
        let scratch = Scratch::new();
        scratch.write("one/.git/HEAD", "ref: refs/heads/one\n");
        scratch.write("two/.git/HEAD", "ref: refs/heads/two\n");
        let now = Instant::now();
        let mut probe = BranchProbe::default();
        probe.sample(Some(&scratch.0.join("one")), now);
        assert_eq!(probe.branch(), Some("one"));
        probe.sample(Some(&scratch.0.join("two")), now);
        assert_eq!(probe.branch(), Some("two"));
        probe.sample(None, now);
        assert_eq!(probe.branch(), None);
    }
}
