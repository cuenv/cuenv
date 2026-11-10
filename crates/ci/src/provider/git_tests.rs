use super::*;
use gix::object::tree::EntryKind;
use gix::refs::transaction::PreviousValue;
use tempfile::TempDir;

/// A repository with an identity, built without running `git`.
fn repository() -> (TempDir, gix::Repository) {
    let directory = tempfile::tempdir().expect("a temporary directory");
    gix::init(directory.path()).expect("a repository");
    let config = directory.path().join(".git/config");
    let mut text = std::fs::read_to_string(&config).expect("the repository's config");
    text.push_str("[user]\n\tname = Ada\n\temail = ada@example.com\n");
    std::fs::write(&config, text).expect("the identity");
    let repository = gix::open(directory.path()).expect("the repository");
    (directory, repository)
}

/// What a test commit does to the tree of its first parent.
enum Edit<'a> {
    Write(&'a str, &'a str),
    Remove(&'a str),
}

/// Commit some edits on top of the given parents and move `HEAD` there.
fn commit(
    repository: &gix::Repository,
    parents: &[gix::ObjectId],
    edits: &[Edit<'_>],
) -> gix::ObjectId {
    let base = parents.first().map_or_else(
        || repository.empty_tree().id,
        |parent| {
            let parent = repository.find_commit(*parent).expect("the parent");
            parent.tree_id().expect("the parent's tree").detach()
        },
    );
    let mut editor = repository.edit_tree(base).expect("a tree editor");
    for edit in edits {
        match edit {
            Edit::Write(path, content) => {
                let blob = repository.write_blob(content).expect("a blob");
                editor
                    .upsert(*path, EntryKind::Blob, blob)
                    .expect("an entry");
            }
            Edit::Remove(path) => {
                editor.remove(*path).expect("a removal");
            }
        }
    }
    let tree = editor.write().expect("a tree");
    let id = repository
        .new_commit("a commit", tree, parents.iter().copied())
        .expect("a commit")
        .id;
    let branch = repository.head_name().expect("HEAD").expect("a branch");
    repository
        .reference(branch, id, PreviousValue::Any, "a test commit")
        .expect("the branch at the commit");
    id
}

fn paths(names: &[&str]) -> Vec<PathBuf> {
    names.iter().map(PathBuf::from).collect()
}

fn sorted(mut files: Vec<PathBuf>) -> Vec<PathBuf> {
    files.sort();
    files
}

#[test]
fn changes_since_a_revision_are_those_up_to_head() {
    let (directory, repository) = repository();
    let first = commit(
        &repository,
        &[],
        &[Edit::Write("a/one.txt", "1"), Edit::Write("b/two.txt", "2")],
    );
    commit(
        &repository,
        &[first],
        &[
            Edit::Write("a/one.txt", "changed"),
            Edit::Write("c/three.txt", "3"),
        ],
    );
    let checkout = Checkout::around(directory.path()).expect("a checkout");

    let changed = checkout.changed(Since::Revision(&first.to_string()));

    assert_eq!(
        changed.map(sorted),
        Some(paths(&["a/one.txt", "c/three.txt"]))
    );
    assert_eq!(
        checkout.changed(Since::Revision("HEAD^")).map(sorted),
        Some(paths(&["a/one.txt", "c/three.txt"]))
    );
}

#[test]
fn changes_since_a_merge_base_leave_out_the_other_side() {
    let (directory, repository) = repository();
    let base = commit(&repository, &[], &[Edit::Write("shared.txt", "0")]);
    let theirs = commit(&repository, &[base], &[Edit::Write("theirs.txt", "1")]);
    commit(&repository, &[base], &[Edit::Write("ours.txt", "2")]);
    let checkout = Checkout::around(directory.path()).expect("a checkout");
    let theirs = theirs.to_string();

    assert_eq!(
        checkout.changed(Since::MergeBaseWith(&theirs)),
        Some(paths(&["ours.txt"]))
    );
    assert_eq!(
        checkout.changed(Since::Revision(&theirs)).map(sorted),
        Some(paths(&["ours.txt", "theirs.txt"]))
    );
}

#[test]
fn a_renamed_file_is_reported_under_both_names() {
    let (directory, repository) = repository();
    let first = commit(
        &repository,
        &[],
        &[Edit::Write("old/file.txt", "the same content")],
    );
    commit(
        &repository,
        &[first],
        &[
            Edit::Remove("old/file.txt"),
            Edit::Write("new/file.txt", "the same content"),
        ],
    );
    let checkout = Checkout::around(directory.path()).expect("a checkout");

    assert_eq!(
        checkout.changed(Since::Revision("HEAD^")).map(sorted),
        Some(paths(&["new/file.txt", "old/file.txt"]))
    );
}

#[test]
fn a_missing_revision_gives_no_answer() {
    let (directory, repository) = repository();
    commit(&repository, &[], &[Edit::Write("one.txt", "1")]);
    let checkout = Checkout::around(directory.path()).expect("a checkout");

    assert_eq!(checkout.changed(Since::Revision("HEAD^")), None);
    assert_eq!(checkout.changed(Since::MergeBaseWith("origin/main")), None);
    assert!(!checkout.has_commit("0123456789012345678901234567890123456789"));
    assert!(checkout.has_commit("HEAD"));
}

#[test]
fn head_is_known_by_its_hash_and_its_parents() {
    let (directory, repository) = repository();
    let first = commit(&repository, &[], &[Edit::Write("one.txt", "1")]);
    let second = commit(&repository, &[first], &[Edit::Write("two.txt", "2")]);
    let checkout = Checkout::around(directory.path()).expect("a checkout");
    assert!(!checkout.head_is_merge());

    let other = commit(&repository, &[first], &[Edit::Write("three.txt", "3")]);
    let merge = commit(
        &repository,
        &[second, other],
        &[Edit::Write("three.txt", "3")],
    );

    assert!(checkout.head_is_merge());
    assert!(checkout.head_is(&merge.to_string().to_uppercase()));
    assert!(!checkout.head_is(&first.to_string()));
}

#[test]
fn uncommitted_changes_are_tracked_files_that_differ_from_head() {
    let (directory, repository) = repository();
    let head = commit(
        &repository,
        &[],
        &[Edit::Write("kept.txt", "1"), Edit::Write("edited.txt", "2")],
    );
    let tree = repository
        .find_commit(head)
        .expect("HEAD")
        .tree_id()
        .expect("its tree");
    let mut index = repository.index_from_tree(&tree).expect("an index");
    index
        .write(gix::index::write::Options::default())
        .expect("the index on disk");
    std::fs::write(directory.path().join("kept.txt"), "1").expect("a clean file");
    std::fs::write(directory.path().join("edited.txt"), "edited").expect("an edited file");
    std::fs::write(directory.path().join("untracked.txt"), "new").expect("an untracked file");
    let checkout = Checkout::around(directory.path()).expect("a checkout");

    assert_eq!(
        checkout.uncommitted().expect("the status"),
        paths(&["edited.txt"])
    );
    assert_eq!(checkout.tracked_files(), paths(&["edited.txt", "kept.txt"]));
    assert!(!checkout.is_shallow());
}

#[test]
fn a_staged_rename_is_uncommitted_under_both_names() {
    let (directory, repository) = repository();
    let first = commit(
        &repository,
        &[],
        &[Edit::Write("old/file.txt", "the same content")],
    );
    let renamed = commit(
        &repository,
        &[first],
        &[
            Edit::Remove("old/file.txt"),
            Edit::Write("new/file.txt", "the same content"),
        ],
    );
    // Stage the rename and leave `HEAD` at the commit before it.
    let staged = repository
        .find_commit(renamed)
        .expect("the rename")
        .tree_id()
        .expect("its tree");
    let mut index = repository.index_from_tree(&staged).expect("an index");
    index
        .write(gix::index::write::Options::default())
        .expect("the index on disk");
    let branch = repository.head_name().expect("HEAD").expect("a branch");
    repository
        .reference(branch, first, PreviousValue::Any, "back before the rename")
        .expect("the branch at the first commit");
    std::fs::create_dir(directory.path().join("new")).expect("a directory");
    std::fs::write(directory.path().join("new/file.txt"), "the same content")
        .expect("the renamed file");
    let checkout = Checkout::around(directory.path()).expect("a checkout");

    assert_eq!(
        checkout.uncommitted().expect("the status"),
        paths(&["new/file.txt", "old/file.txt"])
    );
}

#[test]
fn a_directory_outside_a_repository_is_no_checkout() {
    let directory = tempfile::tempdir().expect("a temporary directory");

    let checkout = temp_env::with_var("GIT_CEILING_DIRECTORIES", directory.path().parent(), || {
        Checkout::around(directory.path())
    });

    assert!(checkout.is_none());
}

/// A repository whose `origin` is another repository on disk. `gix` serves a
/// local remote by running `git upload-pack`; HTTPS remotes stay in process.
fn clone_of(origin: &Path) -> (TempDir, gix::Repository) {
    let (directory, _) = repository();
    let config = directory.path().join(".git/config");
    let mut text = std::fs::read_to_string(&config).expect("the repository's config");
    let remote = format!(
        "[remote \"origin\"]\n\turl = {}\n\tfetch = +refs/heads/*:refs/remotes/origin/*\n",
        origin.display()
    );
    text.push_str(&remote);
    std::fs::write(&config, text).expect("the remote");
    let repository = gix::open(directory.path()).expect("the repository");
    (directory, repository)
}

#[test]
fn fetching_brings_in_what_a_comparison_needs() {
    let (origin_directory, origin) = repository();
    let first = commit(&origin, &[], &[Edit::Write("one.txt", "1")]);
    let second = commit(&origin, &[first], &[Edit::Write("two.txt", "2")]);
    let branch = origin.head_name().expect("HEAD").expect("a branch");
    let branch = branch.shorten().to_string();
    let (directory, _) = clone_of(origin_directory.path());
    let checkout = Checkout::around(directory.path()).expect("a checkout");
    assert!(!checkout.has_commit(&second.to_string()));

    checkout
        .fetch(Fetch::Branch(&branch))
        .expect("a fetched branch");

    assert!(checkout.has_commit(&format!("origin/{branch}")));
    assert!(checkout.is_shallow());
    assert!(!checkout.has_commit(&first.to_string()));

    checkout
        .fetch(Fetch::OneMoreCommit)
        .expect("a deeper history");

    assert!(checkout.has_commit(&first.to_string()));
    assert!(checkout.fetch(Fetch::Branch("no-such-branch")).is_err());
}
