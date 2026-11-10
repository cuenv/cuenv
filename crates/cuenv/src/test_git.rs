//! Git fixtures for tests, built in process: tests run no `git` either.

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};

type Fixture<T = ()> = Result<T, Box<dyn Error>>;

/// A new repository on branch `main` with an identity to commit as.
pub fn init_repository(path: &Path) -> Fixture {
    gix::init(path)?;
    fs::write(path.join(".git/HEAD"), "ref: refs/heads/main\n")?;
    let config = path.join(".git/config");
    let mut text = fs::read_to_string(&config)?;
    text.push_str("[user]\n\tname = Cuenv Test\n\temail = test@example.com\n");
    fs::write(config, text)?;
    Ok(())
}

/// Commit the worktree as it is on top of `HEAD`, as `git add -A` and
/// `git commit` do, and return the commit's hash.
pub fn commit_all(path: &Path, message: &str) -> Fixture<String> {
    let repository = gix::open(path)?;
    let mut editor = repository.edit_tree(repository.empty_tree().id)?;
    for file in files_under(path)? {
        let blob = repository.write_blob(fs::read(&file)?)?;
        let relative = file.strip_prefix(path)?;
        let relative = relative.to_str().ok_or("a path that is not UTF-8")?;
        editor.upsert(relative, gix::object::tree::EntryKind::Blob, blob)?;
    }
    let tree = editor.write()?;
    let parents = repository.head_id().ok().map(gix::Id::detach);
    let commit = repository.commit("HEAD", message, tree, parents)?;
    // The index matches the commit, as it does after `git commit`.
    repository
        .index_from_tree(&tree)?
        .write(gix::index::write::Options::default())?;
    Ok(commit.to_string())
}

/// The files under a directory, leaving out the repository itself.
fn files_under(directory: &Path) -> Fixture<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        if path.ends_with(".git") {
            continue;
        }
        if path.is_dir() {
            files.extend(files_under(&path)?);
        } else {
            files.push(path);
        }
    }
    Ok(files)
}
