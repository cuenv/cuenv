//! The git operations VCS dependency sync needs, done in process with `gix`.
//!
//! cuenv runs no `git`. A remote is reached over its own transport: HTTPS in
//! process, SSH through the `ssh` program and a path on disk through
//! `git upload-pack`, as `gix` reaches them.

use cuenv_core::{Error, Result};
use gix::bstr::ByteSlice;
use gix::object::tree::EntryKind;
use gix::objs::WriteTo;
use gix::refs::transaction::{Change, LogChange, PreviousValue, RefEdit};
use gix::remote::Direction;
use gix::remote::fetch::{Shallow, Tags};
use std::fs;
use std::num::NonZeroU32;
use std::path::Path;
use std::sync::atomic::AtomicBool;

/// A git failure as a configuration error, with every cause it names: the
/// outermost one alone rarely says what went wrong with a remote.
fn failed(error: impl std::error::Error) -> Error {
    let mut message = error.to_string();
    let mut cause = error.source();
    while let Some(error) = cause {
        let detail = error.to_string();
        if !message.contains(&detail) {
            message.push_str(": ");
            message.push_str(&detail);
        }
        cause = error.source();
    }
    problem(message)
}

/// Something git cannot do for us, in words.
fn problem(message: impl std::fmt::Display) -> Error {
    Error::configuration(format!("git: {message}"))
}

/// How much history a fetch brings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum History {
    /// Everything behind what is fetched, and every tag.
    Full,
    /// The fetched commit alone.
    TipOnly,
}

/// One entry of a tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TreeEntry {
    pub name: String,
    /// The type git gives the entry: `tree`, `blob` or `commit`.
    pub kind: &'static str,
    pub id: String,
}

/// The repository at a path, which must be one itself.
pub(super) fn open(path: &Path) -> Result<gix::Repository> {
    gix::open(path).map_err(failed)
}

/// A new, empty repository with a worktree.
pub(super) fn init(path: &Path) -> Result<gix::Repository> {
    gix::init(path).map_err(failed)
}

/// Clone every branch of a remote without checking anything out.
pub(super) fn clone_without_checkout(url: &str, path: &Path) -> Result<gix::Repository> {
    let (repository, _) = gix::prepare_clone(url, path)
        .map_err(failed)?
        .fetch_only(gix::progress::Discard, &AtomicBool::new(false))
        .map_err(failed)?;
    Ok(repository)
}

/// What to fetch from a remote.
#[derive(Debug, Clone, Copy)]
pub(super) struct Fetch<'a> {
    pub url: &'a str,
    /// A branch, a tag or a commit hash.
    pub reference: &'a str,
    pub history: History,
}

/// Fetch from a remote, and return the hash of the commit the reference
/// names.
pub(super) fn fetch(repository: &gix::Repository, fetch: Fetch<'_>) -> Result<String> {
    let Fetch {
        url,
        reference,
        history,
    } = fetch;
    let (tags, shallow) = match history {
        History::Full => (Tags::All, Shallow::NoChange),
        History::TipOnly => (Tags::None, Shallow::DepthAtRemote(NonZeroU32::MIN)),
    };
    let outcome = repository
        .remote_at(url)
        .map_err(failed)?
        .with_fetch_tags(tags)
        .with_refspecs(Some(reference), Direction::Fetch)
        .map_err(failed)?
        .connect(Direction::Fetch)
        .map_err(failed)?
        .prepare_fetch(
            gix::progress::Discard,
            gix::remote::ref_map::Options::default(),
        )
        .map_err(failed)?
        .with_shallow(shallow)
        .receive(gix::progress::Discard, &AtomicBool::new(false))
        .map_err(failed)?;
    let fetched = outcome
        .ref_map
        .mappings
        .iter()
        .find(|mapping| mapping.spec_index.implicit_index().is_none())
        .and_then(|mapping| mapping.remote.as_id())
        .ok_or_else(|| problem(format!("'{reference}' was not found at '{url}'")))?;
    let commit = repository
        .find_object(fetched)
        .map_err(failed)?
        .peel_to_commit()
        .map_err(failed)?;
    Ok(commit.id.to_string())
}

/// The commit `HEAD` is at.
pub(super) fn head_commit(repository: &gix::Repository) -> Result<String> {
    let head = repository.head_id().map_err(failed)?;
    Ok(head.to_string())
}

/// The tree of a commit.
pub(super) fn tree_of(repository: &gix::Repository, commit: &str) -> Result<String> {
    let tree = find_commit(repository, commit)?.tree_id().map_err(failed)?;
    Ok(tree.to_string())
}

/// The entry at a path of a commit's tree; `None` when there is none.
pub(super) fn entry_at(
    repository: &gix::Repository,
    commit: &str,
    path: &str,
) -> Result<Option<TreeEntry>> {
    let tree = find_commit(repository, commit)?.tree().map_err(failed)?;
    let entry = tree.lookup_entry_by_path(path).map_err(failed)?;
    Ok(entry.map(|entry| TreeEntry {
        name: entry.filename().to_string(),
        kind: object_type(entry.mode().kind()),
        id: entry.object_id().to_string(),
    }))
}

/// The immediate entries of a tree.
pub(super) fn entries(repository: &gix::Repository, tree: &str) -> Result<Vec<TreeEntry>> {
    let tree = find_tree(repository, tree)?;
    tree.iter()
        .map(|entry| {
            let entry = entry.map_err(failed)?;
            let name = entry.filename().to_str().map_err(|_| {
                problem(format!(
                    "entry '{}' does not have a UTF-8 name",
                    entry.filename()
                ))
            })?;
            Ok(TreeEntry {
                name: name.to_string(),
                kind: object_type(entry.mode().kind()),
                id: entry.object_id().to_string(),
            })
        })
        .collect()
}

/// Check a commit out into the repository's worktree and detach `HEAD` at it,
/// as `git checkout --detach` does into an empty worktree.
pub(super) fn checkout_detached(repository: &gix::Repository, commit: &str) -> Result<()> {
    let commit = find_commit(repository, commit)?;
    let tree = commit.tree_id().map_err(failed)?;
    let worktree = repository
        .workdir()
        .ok_or_else(|| problem("the repository has no worktree"))?;
    let mut index = repository.index_from_tree(&tree).map_err(failed)?;
    let mut options = repository
        .checkout_options(gix::worktree::stack::state::attributes::Source::IdMapping)
        .map_err(failed)?;
    options.destination_is_initially_empty = true;
    let outcome = gix::worktree::state::checkout(
        &mut index,
        worktree,
        repository.objects.clone().into_arc().map_err(failed)?,
        &gix::progress::Discard,
        &gix::progress::Discard,
        &AtomicBool::new(false),
        options,
    )
    .map_err(failed)?;
    if let Some(error) = outcome.errors.first() {
        return Err(problem(format!(
            "could not check out '{}': {}",
            error.path, error.error
        )));
    }
    if let Some(collision) = outcome.collisions.first() {
        return Err(problem(format!(
            "'{}' collides with another path on this filesystem",
            collision.path
        )));
    }
    index
        .write(gix::index::write::Options::default())
        .map_err(failed)?;
    repository
        .edit_reference(RefEdit {
            change: Change::Update {
                log: LogChange::default(),
                expected: PreviousValue::Any,
                new: gix::refs::Target::Object(commit.id),
            },
            name: "HEAD".try_into().map_err(failed)?,
            deref: false,
        })
        .map_err(failed)?;
    Ok(())
}

/// Write the files of a tree into a new directory, byte for byte as git
/// stores them. A submodule is left out, as an unpopulated one is.
pub(super) fn export_tree(
    repository: &gix::Repository,
    tree: &str,
    destination: &Path,
) -> Result<()> {
    let tree = find_tree(repository, tree)?;
    // Building the index refuses names that would escape the destination.
    let index = repository.index_from_tree(&tree.id).map_err(failed)?;
    fs::create_dir(destination).map_err(failed)?;
    for entry in index.entries() {
        let Some(kind) = entry.mode.to_tree_entry_mode().map(|mode| mode.kind()) else {
            continue;
        };
        if kind == EntryKind::Commit {
            continue;
        }
        let path = destination.join(gix::path::from_bstr(entry.path(&index)));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(failed)?;
        }
        let blob = repository.find_blob(entry.id).map_err(failed)?;
        if kind == EntryKind::Link {
            write_link(&blob.data, &path)?;
        } else {
            fs::write(&path, &blob.data).map_err(failed)?;
            if kind == EntryKind::BlobExecutable {
                make_executable(&path)?;
            }
        }
    }
    Ok(())
}

/// The paths of a checkout that differ from `HEAD`, are untracked or are
/// ignored: what `git status --ignored` lists.
pub(super) fn changes(repository: &gix::Repository) -> Result<Vec<String>> {
    let changes = repository
        .status(gix::progress::Discard)
        .map_err(failed)?
        .untracked_files(gix::status::UntrackedFiles::Files)
        .dirwalk_options(|options| {
            options.emit_ignored(Some(gix::dir::walk::EmissionMode::Matching))
        })
        .into_iter(None)
        .map_err(failed)?;
    changes
        .map(|change| Ok(change.map_err(failed)?.location().to_string()))
        .collect()
}

/// The hash of the tree git would store for a directory with every file in
/// it added, leaving out one name at the top. `.git` and empty directories
/// are not part of a tree.
pub(super) fn directory_tree(directory: &Path, without: &str) -> Result<String> {
    let kind = gix::hash::Kind::Sha1;
    let tree = hash_directory(directory, Some(without))?
        .unwrap_or_else(|| gix::ObjectId::empty_tree(kind));
    Ok(tree.to_string())
}

/// The hash of a directory's tree; `None` when it holds no file.
fn hash_directory(directory: &Path, without: Option<&str>) -> Result<Option<gix::ObjectId>> {
    let kind = gix::hash::Kind::Sha1;
    let hash = |object, data: &[u8]| gix::objs::compute_hash(kind, object, data).map_err(failed);
    let mut entries = Vec::new();
    for entry in fs::read_dir(directory).map_err(failed)? {
        let entry = entry.map_err(failed)?;
        let name = entry.file_name();
        if name == ".git" || without.is_some_and(|without| name == without) {
            continue;
        }
        let path = entry.path();
        let metadata = entry.metadata().map_err(failed)?;
        let (mode, oid) = if metadata.is_symlink() {
            let target = fs::read_link(&path).map_err(failed)?;
            let target = gix::path::into_bstr(target);
            (EntryKind::Link, hash(gix::objs::Kind::Blob, &target)?)
        } else if metadata.is_dir() {
            match hash_directory(&path, None)? {
                Some(tree) => (EntryKind::Tree, tree),
                None => continue,
            }
        } else {
            let content = fs::read(&path).map_err(failed)?;
            let mode = if is_executable(&metadata) {
                EntryKind::BlobExecutable
            } else {
                EntryKind::Blob
            };
            (mode, hash(gix::objs::Kind::Blob, &content)?)
        };
        entries.push(gix::objs::tree::Entry {
            mode: mode.into(),
            filename: gix::path::os_str_into_bstr(&name)
                .map_err(failed)?
                .to_owned(),
            oid,
        });
    }
    if entries.is_empty() {
        return Ok(None);
    }
    entries.sort();
    let mut data = Vec::new();
    gix::objs::Tree { entries }
        .write_to(&mut data)
        .map_err(failed)?;
    hash(gix::objs::Kind::Tree, &data).map(Some)
}

fn find_commit<'repository>(
    repository: &'repository gix::Repository,
    commit: &str,
) -> Result<gix::Commit<'repository>> {
    let id = gix::ObjectId::from_hex(commit.as_bytes()).map_err(failed)?;
    repository.find_commit(id).map_err(failed)
}

fn find_tree<'repository>(
    repository: &'repository gix::Repository,
    tree: &str,
) -> Result<gix::Tree<'repository>> {
    let id = gix::ObjectId::from_hex(tree.as_bytes()).map_err(failed)?;
    repository.find_tree(id).map_err(failed)
}

/// The type git gives a tree entry.
const fn object_type(kind: EntryKind) -> &'static str {
    match kind {
        EntryKind::Tree => "tree",
        EntryKind::Blob | EntryKind::BlobExecutable | EntryKind::Link => "blob",
        EntryKind::Commit => "commit",
    }
}

#[cfg(unix)]
fn write_link(target: &[u8], path: &Path) -> Result<()> {
    std::os::unix::fs::symlink(gix::path::from_byte_slice(target), path).map_err(failed)
}

/// Without symbolic links, a link is a file holding its target, as git
/// checks it out when `core.symlinks` is off.
#[cfg(not(unix))]
fn write_link(target: &[u8], path: &Path) -> Result<()> {
    fs::write(path, target).map_err(failed)
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path).map_err(failed)?.permissions();
    permissions.set_mode(permissions.mode() | 0o111);
    fs::set_permissions(path, permissions).map_err(failed)
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn is_executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &fs::Metadata) -> bool {
    false
}
