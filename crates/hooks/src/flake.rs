//! Locating the flake Nix resolves for a bare reference.
//!
//! `nix print-dev-env .` does not require a `flake.nix` in the current
//! directory: Nix searches the parent directories for the nearest one and
//! stops at the root of the repository. cuenv has to look at the same
//! directory when it asks which files a Nix shell depends on.

use std::path::{Path, PathBuf};

/// The nearest directory at or above `start` that holds a `flake.nix`.
///
/// The search ends after `ceiling` when one is given, and after the first
/// directory that holds a `.git` entry or a `cue.mod` directory, so it never
/// leaves the repository or the CUE module. A `start` that is not below
/// `ceiling` is not searched above.
#[must_use]
pub fn nearest_flake_root(start: &Path, ceiling: Option<&Path>) -> Option<PathBuf> {
    for dir in start.ancestors() {
        if dir.join("flake.nix").is_file() {
            return Some(dir.to_path_buf());
        }
        if ceiling.is_some_and(|ceiling| dir == ceiling)
            || dir.join(".git").exists()
            || dir.join("cue.mod").is_dir()
        {
            return None;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::nearest_flake_root;

    #[test]
    fn finds_the_nearest_flake_and_stops_at_the_ceiling() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project = temp.path().join("a/b");
        std::fs::create_dir_all(&project).expect("mkdir");
        std::fs::write(temp.path().join("flake.nix"), "{}").expect("flake");

        assert_eq!(
            nearest_flake_root(&project, None),
            Some(temp.path().to_path_buf())
        );
        assert_eq!(
            nearest_flake_root(&project, Some(&temp.path().join("a"))),
            None
        );

        std::fs::write(project.join("flake.nix"), "{}").expect("flake");
        assert_eq!(nearest_flake_root(&project, None), Some(project));
    }

    #[test]
    fn stops_at_the_repository_root() {
        let temp = tempfile::tempdir().expect("tempdir");
        let repo = temp.path().join("repo");
        let project = repo.join("p");
        std::fs::create_dir_all(&project).expect("mkdir");
        std::fs::create_dir_all(repo.join(".git")).expect("git");
        std::fs::write(temp.path().join("flake.nix"), "{}").expect("flake");

        assert_eq!(nearest_flake_root(&project, None), None);
    }
}
