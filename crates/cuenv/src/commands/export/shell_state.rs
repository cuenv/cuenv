//! Names-only state for restoring an interactive shell's environment.

use cuenv_core::{Error, Result};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::path::Path;
use uuid::Uuid;

const TOKEN_LIMIT: usize = 24 * 1024;
const NAME_LIMIT: usize = 1024;

/// Public metadata carried between shell prompts. Original values stay in the shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ShellState {
    pub(super) nonce: Uuid,
    pub(super) project: String,
    pub(super) names: Vec<String>,
}

impl ShellState {
    pub(super) fn parse(token: &str) -> Result<Self> {
        if token.len() > TOKEN_LIMIT {
            return Err(invalid_state());
        }
        let mut fields = token.split(':');
        if fields.next() != Some("v1") {
            return Err(invalid_state());
        }
        let nonce_text = fields.next().ok_or_else(invalid_state)?;
        let nonce = Uuid::parse_str(nonce_text).map_err(|_| invalid_state())?;
        if nonce.get_version_num() != 4 || nonce.to_string() != nonce_text {
            return Err(invalid_state());
        }
        let project = fields.next().ok_or_else(invalid_state)?;
        if project.len() != 64
            || !project
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(invalid_state());
        }
        let names_text = fields.next().ok_or_else(invalid_state)?;
        if fields.next().is_some() {
            return Err(invalid_state());
        }
        let mut seen = HashSet::new();
        let mut names = Vec::new();
        if !names_text.is_empty() {
            for name in names_text.split(',') {
                if names.len() == NAME_LIMIT
                    || !super::implementation::is_allowed_shell_name(name)
                    || !seen.insert(name)
                {
                    return Err(invalid_state());
                }
                names.push(name.to_string());
            }
        }
        Ok(Self {
            nonce,
            project: project.to_string(),
            names,
        })
    }

    pub(super) fn for_project(directory: &Path, package: &str) -> Result<String> {
        let canonical = directory.canonicalize().map_err(|error| {
            Error::io_with_path(
                "canonicalize project directory",
                directory.to_path_buf(),
                error,
            )
        })?;
        let path = canonical.as_os_str().as_encoded_bytes();
        let mut hash = Sha256::new();
        hash.update(b"cuenv-shell-project-v1\0");
        hash.update(
            u64::try_from(path.len())
                .map_err(|_| invalid_state())?
                .to_be_bytes(),
        );
        hash.update(path);
        hash.update(
            u64::try_from(package.len())
                .map_err(|_| invalid_state())?
                .to_be_bytes(),
        );
        hash.update(package.as_bytes());
        Ok(hex::encode(hash.finalize()))
    }

    pub(super) fn token(&self) -> Result<String> {
        let token = format!(
            "v1:{}:{}:{}",
            self.nonce,
            self.project,
            self.names.join(",")
        );
        let parsed = Self::parse(&token)?;
        if parsed == *self {
            Ok(token)
        } else {
            Err(invalid_state())
        }
    }
}

fn invalid_state() -> Error {
    Error::configuration(
        "Shell restoration state is invalid or unavailable; start a fresh shell or explicitly clear the inherited environment",
    )
}

#[cfg(test)]
mod tests {
    use super::ShellState;

    #[test]
    fn rejects_malformed_duplicate_reserved_and_oversized_tokens() {
        let nonce = uuid::Uuid::new_v4();
        let project = "a".repeat(64);
        for names in [
            "VALUE,VALUE",
            "VALUE,CUENV_HOOK_SESSION",
            "X;command",
            "VALUE,,OTHER",
        ] {
            assert!(ShellState::parse(&format!("v1:{nonce}:{project}:{names}")).is_err());
        }
        assert!(ShellState::parse(&"x".repeat(25 * 1024)).is_err());
        assert!(ShellState::parse(&format!("v1:{nonce}:{project}:VALUE")).is_ok());
    }

    #[test]
    fn binds_the_canonical_directory_and_package() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let first = ShellState::for_project(directory.path(), "cuenv")?;
        let second = ShellState::for_project(directory.path(), "other")?;
        assert_ne!(first, second);
        let state = ShellState {
            nonce: uuid::Uuid::new_v4(),
            project: first,
            names: vec!["FOO".to_string()],
        };
        assert_eq!(ShellState::parse(&state.token()?)?, state);
        Ok(())
    }
}
