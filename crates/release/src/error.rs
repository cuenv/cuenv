//! Error types for release management operations.

use miette::Diagnostic;
use std::path::PathBuf;
use thiserror::Error;

/// Result type alias for release operations.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors that can occur during release management operations.
#[derive(Error, Debug, Diagnostic)]
pub enum Error {
    /// Failed to read or write a changeset file.
    #[error("Changeset I/O error: {message}")]
    #[diagnostic(
        code(cuenv::release::changeset_io),
        help("Check that the .cuenv/changesets directory exists and is writable")
    )]
    ChangesetIo {
        /// The error message
        message: String,
        /// The path that caused the error
        path: Option<PathBuf>,
        /// The underlying source error
        #[source]
        source: Option<std::io::Error>,
    },

    /// Failed to parse a changeset file.
    #[error("Invalid changeset format: {message}")]
    #[diagnostic(
        code(cuenv::release::changeset_parse),
        help("Ensure the changeset file is valid Markdown with proper frontmatter")
    )]
    ChangesetParse {
        /// The error message
        message: String,
        /// The path to the invalid file
        path: Option<PathBuf>,
    },

    /// Failed to parse or validate a version string.
    #[error("Invalid version: {version}")]
    #[diagnostic(
        code(cuenv::release::invalid_version),
        help("Version must follow semantic versioning (e.g., 1.0.0, 2.1.0-beta.1)")
    )]
    InvalidVersion {
        /// The invalid version string
        version: String,
    },

    /// Package not found in the workspace.
    #[error("Package not found: {name}")]
    #[diagnostic(
        code(cuenv::release::package_not_found),
        help("Ensure the package exists in the workspace and is properly configured")
    )]
    PackageNotFound {
        /// The package name that wasn't found
        name: String,
    },

    /// No changesets found for release.
    #[error("No changesets found")]
    #[diagnostic(
        code(cuenv::release::no_changesets),
        help("Create changesets with 'cuenv changeset add' before running release version")
    )]
    NoChangesets,

    /// Configuration error.
    #[error("Release configuration error: {message}")]
    #[diagnostic(code(cuenv::release::config), help("{help}"))]
    Config {
        /// The error message
        message: String,
        /// Help text for the user
        help: String,
    },

    /// Manifest file error (Cargo.toml, package.json, etc.).
    #[error("Manifest error: {message}")]
    #[diagnostic(
        code(cuenv::release::manifest),
        help("Check that the manifest file exists and is properly formatted")
    )]
    Manifest {
        /// The error message
        message: String,
        /// The manifest file path
        path: Option<PathBuf>,
    },

    /// Git operation error.
    #[error("Git error: {message}")]
    #[diagnostic(
        code(cuenv::release::git),
        help("Ensure you are in a git repository and have the necessary permissions")
    )]
    Git {
        /// The error message
        message: String,
    },

    /// Publish error.
    #[error("Publish failed: {message}")]
    #[diagnostic(code(cuenv::release::publish))]
    Publish {
        /// The error message
        message: String,
        /// The package that failed to publish
        package: Option<String>,
    },

    /// Wrapped I/O error.
    #[error("I/O error: {0}")]
    #[diagnostic(code(cuenv::release::io))]
    Io(#[from] std::io::Error),

    /// Wrapped JSON error.
    #[error("JSON error: {0}")]
    #[diagnostic(code(cuenv::release::json))]
    Json(#[from] serde_json::Error),

    /// Wrapped TOML parsing error.
    #[error("TOML parse error: {0}")]
    #[diagnostic(code(cuenv::release::toml_parse))]
    TomlParse(#[from] toml::de::Error),

    /// Wrapped TOML serialization error.
    #[error("TOML serialization error: {0}")]
    #[diagnostic(code(cuenv::release::toml_ser))]
    TomlSer(#[from] toml::ser::Error),
}

impl Error {
    /// Create a new changeset I/O error.
    #[must_use]
    pub fn changeset_io(message: impl Into<String>, path: Option<PathBuf>) -> Self {
        Self::ChangesetIo {
            message: message.into(),
            path,
            source: None,
        }
    }

    /// Create a new changeset I/O error with source.
    #[must_use]
    pub fn changeset_io_with_source(
        message: impl Into<String>,
        path: Option<PathBuf>,
        source: std::io::Error,
    ) -> Self {
        Self::ChangesetIo {
            message: message.into(),
            path,
            source: Some(source),
        }
    }

    /// Create a new changeset parse error.
    #[must_use]
    pub fn changeset_parse(message: impl Into<String>, path: Option<PathBuf>) -> Self {
        Self::ChangesetParse {
            message: message.into(),
            path,
        }
    }

    /// Create a new invalid version error.
    #[must_use]
    pub fn invalid_version(version: impl Into<String>) -> Self {
        Self::InvalidVersion {
            version: version.into(),
        }
    }

    /// Create a new package not found error.
    #[must_use]
    pub fn package_not_found(name: impl Into<String>) -> Self {
        Self::PackageNotFound { name: name.into() }
    }

    /// Create a new configuration error.
    #[must_use]
    pub fn config(message: impl Into<String>, help: impl Into<String>) -> Self {
        Self::Config {
            message: message.into(),
            help: help.into(),
        }
    }

    /// Create a new manifest error.
    #[must_use]
    pub fn manifest(message: impl Into<String>, path: Option<PathBuf>) -> Self {
        Self::Manifest {
            message: message.into(),
            path,
        }
    }

    /// Create a new git error.
    #[must_use]
    pub fn git(message: impl Into<String>) -> Self {
        Self::Git {
            message: message.into(),
        }
    }

    /// Create a new publish error.
    #[must_use]
    pub fn publish(message: impl Into<String>, package: Option<String>) -> Self {
        Self::Publish {
            message: message.into(),
            package,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_changeset_io_error() {
        let err = Error::changeset_io("failed to write", Some(PathBuf::from(".cuenv/test.md")));
        assert!(err.to_string().contains("Changeset I/O error"));
    }

    #[test]
    fn test_changeset_parse_error() {
        let err = Error::changeset_parse("invalid frontmatter", None);
        assert!(err.to_string().contains("Invalid changeset format"));
    }

    #[test]
    fn test_invalid_version_error() {
        let err = Error::invalid_version("not-a-version");
        assert!(err.to_string().contains("not-a-version"));
    }

    #[test]
    fn test_package_not_found_error() {
        let err = Error::package_not_found("missing-pkg");
        assert!(err.to_string().contains("missing-pkg"));
    }

    #[test]
    fn test_config_error() {
        let err = Error::config("bad config", "check your settings");
        assert!(err.to_string().contains("bad config"));
    }

    #[test]
    fn test_manifest_error() {
        let err = Error::manifest("invalid toml", Some(PathBuf::from("Cargo.toml")));
        assert!(err.to_string().contains("Manifest error"));
    }

    #[test]
    fn test_git_error() {
        let err = Error::git("not a repository");
        assert!(err.to_string().contains("Git error"));
    }

    #[test]
    fn test_publish_error() {
        let err = Error::publish("auth failed", Some("my-pkg".to_string()));
        assert!(err.to_string().contains("Publish failed"));
    }

    #[test]
    fn test_no_changesets_error() {
        let err = Error::NoChangesets;
        assert!(err.to_string().contains("No changesets found"));
    }
}
