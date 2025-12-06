//! Error types for workspace operations.

use miette::Diagnostic;
use std::path::PathBuf;
use thiserror::Error;

/// Result type for workspace operations.
pub type Result<T> = std::result::Result<T, Error>;

/// Errors that can occur during workspace operations.
#[derive(Error, Debug, Diagnostic)]
pub enum Error {
    /// Workspace root directory not found.
    #[error("Workspace not found at path: {path}")]
    #[diagnostic(
        code(cuenv::workspaces::workspace_not_found),
        help(
            "Ensure the path points to a valid workspace root directory with a workspace configuration file"
        )
    )]
    WorkspaceNotFound {
        /// The path that was searched.
        path: PathBuf,
    },

    /// Invalid workspace configuration.
    #[error("Invalid workspace configuration at {path}: {message}")]
    #[diagnostic(
        code(cuenv::workspaces::invalid_config),
        help(
            "Check the workspace configuration file for syntax errors or missing required fields"
        )
    )]
    InvalidWorkspaceConfig {
        /// Path to the invalid configuration file.
        path: PathBuf,
        /// Description of what is invalid.
        message: String,
    },

    /// Lockfile not found.
    #[error("Lockfile not found at path: {path}")]
    #[diagnostic(
        code(cuenv::workspaces::lockfile_not_found),
        help(
            "Run your package manager's install command to generate a lockfile (e.g., 'npm install', 'cargo build')"
        )
    )]
    LockfileNotFound {
        /// The path where the lockfile was expected.
        path: PathBuf,
    },

    /// Manifest file not found.
    #[error("Manifest file not found at path: {path}")]
    #[diagnostic(
        code(cuenv::workspaces::manifest_not_found),
        help(
            "Ensure the manifest file exists at the expected location (e.g., 'package.json', 'Cargo.toml')"
        )
    )]
    ManifestNotFound {
        /// The path where the manifest was expected.
        path: PathBuf,
    },

    /// Failed to parse lockfile.
    #[error("Failed to parse lockfile at {path}: {message}")]
    #[diagnostic(
        code(cuenv::workspaces::lockfile_parse_failed),
        help("The lockfile may be corrupted. Try regenerating it with your package manager")
    )]
    LockfileParseFailed {
        /// Path to the lockfile.
        path: PathBuf,
        /// Description of the parse error.
        message: String,
    },

    /// Workspace member not found.
    #[error("Workspace member '{name}' not found in workspace at {workspace_root}")]
    #[diagnostic(
        code(cuenv::workspaces::member_not_found),
        help(
            "Check that the member name is correct and the member is listed in the workspace configuration"
        )
    )]
    MemberNotFound {
        /// Name of the missing member.
        name: String,
        /// Root of the workspace where the member was expected.
        workspace_root: PathBuf,
    },

    /// Dependency resolution failed.
    #[error("Failed to resolve dependencies: {message}")]
    #[diagnostic(
        code(cuenv::workspaces::dependency_resolution_failed),
        help("Check for circular dependencies or missing dependencies in the lockfile")
    )]
    DependencyResolutionFailed {
        /// Description of the resolution failure.
        message: String,
    },

    /// Unsupported package manager.
    #[error("Unsupported package manager: {manager}")]
    #[diagnostic(
        code(cuenv::workspaces::unsupported_manager),
        help("Supported package managers: npm, bun, pnpm, yarn, cargo")
    )]
    UnsupportedPackageManager {
        /// The unsupported package manager name.
        manager: String,
    },

    /// I/O error occurred.
    #[error("I/O error during {operation}{}: {source}", path.as_ref().map(|p| format!(" at {}", p.display())).unwrap_or_default())]
    #[diagnostic(
        code(cuenv::workspaces::io_error),
        help(
            "Check that the referenced paths exist and that you have permission to read or write them"
        )
    )]
    Io {
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
        /// Optional path where the error occurred.
        path: Option<PathBuf>,
        /// Description of the operation being performed.
        operation: String,
    },

    /// JSON parsing error.
    #[error("JSON parsing error{}: {source}", path.as_ref().map(|p| format!(" in {}", p.display())).unwrap_or_default())]
    #[diagnostic(
        code(cuenv::workspaces::json_error),
        help(
            "Ensure the JSON has valid syntax and matches the expected schema for workspace metadata"
        )
    )]
    Json {
        /// The underlying JSON error.
        #[source]
        source: serde_json::Error,
        /// Optional path to the file being parsed.
        path: Option<PathBuf>,
    },

    /// YAML parsing error.
    #[cfg(feature = "serde_yaml")]
    #[error("YAML parsing error{}: {source}", path.as_ref().map(|p| format!(" in {}", p.display())).unwrap_or_default())]
    #[diagnostic(
        code(cuenv::workspaces::yaml_error),
        help(
            "Ensure the YAML has valid syntax and matches the expected schema for workspace metadata"
        )
    )]
    Yaml {
        /// The underlying YAML error.
        #[source]
        source: serde_yaml::Error,
        /// Optional path to the file being parsed.
        path: Option<PathBuf>,
    },

    /// TOML parsing error.
    #[cfg(feature = "toml")]
    #[error("TOML parsing error{}: {source}", path.as_ref().map(|p| format!(" in {}", p.display())).unwrap_or_default())]
    #[diagnostic(
        code(cuenv::workspaces::toml_error),
        help(
            "Ensure the TOML has valid syntax and matches the expected schema for Cargo manifests"
        )
    )]
    Toml {
        /// The underlying TOML error.
        #[source]
        source: toml::de::Error,
        /// Optional path to the file being parsed.
        path: Option<PathBuf>,
    },
}

impl From<std::io::Error> for Error {
    fn from(source: std::io::Error) -> Self {
        Self::Io {
            source,
            path: None,
            operation: "file operation".to_string(),
        }
    }
}

impl From<serde_json::Error> for Error {
    fn from(source: serde_json::Error) -> Self {
        Self::Json { source, path: None }
    }
}

#[cfg(feature = "serde_yaml")]
impl From<serde_yaml::Error> for Error {
    fn from(source: serde_yaml::Error) -> Self {
        Self::Yaml { source, path: None }
    }
}

#[cfg(feature = "toml")]
impl From<toml::de::Error> for Error {
    fn from(source: toml::de::Error) -> Self {
        Self::Toml { source, path: None }
    }
}

#[cfg(test)]
#[allow(clippy::unnecessary_wraps)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn test_workspace_not_found_error() {
        let error = Error::WorkspaceNotFound {
            path: PathBuf::from("/nonexistent"),
        };

        let message = error.to_string();
        assert!(message.contains("Workspace not found"));
        assert!(message.contains("/nonexistent"));
    }

    #[test]
    fn test_invalid_workspace_config_error() {
        let error = Error::InvalidWorkspaceConfig {
            path: PathBuf::from("/workspace/package.json"),
            message: "Missing 'workspaces' field".to_string(),
        };

        let message = error.to_string();
        assert!(message.contains("Invalid workspace configuration"));
        assert!(message.contains("package.json"));
        assert!(message.contains("Missing 'workspaces' field"));
    }

    #[test]
    fn test_lockfile_not_found_error() {
        let error = Error::LockfileNotFound {
            path: PathBuf::from("/workspace/package-lock.json"),
        };

        let message = error.to_string();
        assert!(message.contains("Lockfile not found"));
        assert!(message.contains("package-lock.json"));
    }

    #[test]
    fn test_lockfile_parse_failed_error() {
        let error = Error::LockfileParseFailed {
            path: PathBuf::from("/workspace/Cargo.lock"),
            message: "Invalid TOML syntax".to_string(),
        };

        let message = error.to_string();
        assert!(message.contains("Failed to parse lockfile"));
        assert!(message.contains("Cargo.lock"));
        assert!(message.contains("Invalid TOML syntax"));
    }

    #[test]
    fn test_member_not_found_error() {
        let error = Error::MemberNotFound {
            name: "my-package".to_string(),
            workspace_root: PathBuf::from("/workspace"),
        };

        let message = error.to_string();
        assert!(message.contains("Workspace member"));
        assert!(message.contains("my-package"));
        assert!(message.contains("not found"));
    }

    #[test]
    fn test_dependency_resolution_failed_error() {
        let error = Error::DependencyResolutionFailed {
            message: "Circular dependency detected".to_string(),
        };

        let message = error.to_string();
        assert!(message.contains("Failed to resolve dependencies"));
        assert!(message.contains("Circular dependency"));
    }

    #[test]
    fn test_unsupported_package_manager_error() {
        let error = Error::UnsupportedPackageManager {
            manager: "poetry".to_string(),
        };

        let message = error.to_string();
        assert!(message.contains("Unsupported package manager"));
        assert!(message.contains("poetry"));
    }

    #[test]
    fn test_io_error_display() {
        let io_error = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let error = Error::Io {
            source: io_error,
            path: Some(PathBuf::from("/test/file.txt")),
            operation: "reading file".to_string(),
        };

        let message = error.to_string();
        assert!(message.contains("I/O error during reading file"));
        assert!(message.contains("/test/file.txt"));
    }

    #[test]
    fn test_io_error_no_path() {
        let io_error = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "access denied");
        let error = Error::Io {
            source: io_error,
            path: None,
            operation: "opening directory".to_string(),
        };

        let message = error.to_string();
        assert!(message.contains("I/O error during opening directory"));
        assert!(!message.contains(" at "));
    }

    #[test]
    fn test_json_error_display() {
        let json_str = "{ invalid json }";
        let json_error = serde_json::from_str::<serde_json::Value>(json_str).unwrap_err();
        let error = Error::Json {
            source: json_error,
            path: Some(PathBuf::from("/workspace/package.json")),
        };

        let message = error.to_string();
        assert!(message.contains("JSON parsing error"));
        assert!(message.contains("package.json"));
    }

    #[test]
    fn test_json_error_no_path() {
        let json_str = "{ invalid json }";
        let json_error = serde_json::from_str::<serde_json::Value>(json_str).unwrap_err();
        let error = Error::Json {
            source: json_error,
            path: None,
        };

        let message = error.to_string();
        assert!(message.contains("JSON parsing error"));
        assert!(!message.contains(" in "));
    }

    #[test]
    fn test_io_error_conversion() {
        let io_error = std::io::Error::new(std::io::ErrorKind::NotFound, "not found");
        let error: Error = io_error.into();

        match error {
            Error::Io {
                source: _,
                path,
                operation,
            } => {
                assert_eq!(path, None);
                assert_eq!(operation, "file operation");
            }
            _ => panic!("Expected Io error variant"),
        }
    }

    #[test]
    fn test_json_error_conversion() {
        let json_error = serde_json::from_str::<serde_json::Value>("invalid").unwrap_err();
        let error: Error = json_error.into();

        match error {
            Error::Json { source: _, path } => {
                assert_eq!(path, None);
            }
            _ => panic!("Expected Json error variant"),
        }
    }

    #[cfg(feature = "serde_yaml")]
    #[test]
    fn test_yaml_error_display() {
        let yaml_error = serde_yaml::from_str::<serde_yaml::Value>("invalid: : yaml").unwrap_err();
        let error = Error::Yaml {
            source: yaml_error,
            path: Some(PathBuf::from("/workspace/pnpm-lock.yaml")),
        };

        let message = error.to_string();
        assert!(message.contains("YAML parsing error"));
        assert!(message.contains("pnpm-lock.yaml"));
    }

    #[cfg(feature = "serde_yaml")]
    #[test]
    fn test_yaml_error_no_path() {
        let yaml_error = serde_yaml::from_str::<serde_yaml::Value>("invalid: : yaml").unwrap_err();
        let error = Error::Yaml {
            source: yaml_error,
            path: None,
        };

        let message = error.to_string();
        assert!(message.contains("YAML parsing error"));
        // Check that it doesn't have the path context prefix
        assert!(message.starts_with("YAML parsing error: "));
    }

    #[cfg(feature = "serde_yaml")]
    #[test]
    fn test_yaml_error_conversion() {
        let yaml_error = serde_yaml::from_str::<serde_yaml::Value>("invalid: : yaml").unwrap_err();
        let error: Error = yaml_error.into();

        match error {
            Error::Yaml { source: _, path } => {
                assert_eq!(path, None);
            }
            _ => panic!("Expected Yaml error variant"),
        }
    }

    #[cfg(feature = "serde_yaml")]
    #[test]
    fn test_yaml_error_diagnostics() {
        use miette::Diagnostic;

        let yaml_error = serde_yaml::from_str::<serde_yaml::Value>("invalid: : yaml").unwrap_err();
        let error = Error::Yaml {
            source: yaml_error,
            path: None,
        };

        assert_eq!(
            error.code().map(|c| c.to_string()),
            Some("cuenv::workspaces::yaml_error".to_string())
        );
        assert!(error.help().is_some());
    }

    #[cfg(feature = "toml")]
    #[test]
    fn test_toml_error_display() {
        let toml_error = toml::from_str::<toml::Value>("not valid = [").unwrap_err();
        let error = Error::Toml {
            source: toml_error,
            path: Some(PathBuf::from("/workspace/Cargo.toml")),
        };

        let message = error.to_string();
        assert!(message.contains("TOML parsing error"));
        assert!(message.contains("Cargo.toml"));
    }

    #[cfg(feature = "toml")]
    #[test]
    fn test_toml_error_no_path() {
        let toml_error = toml::from_str::<toml::Value>("not valid = [").unwrap_err();
        let error = Error::Toml {
            source: toml_error,
            path: None,
        };

        let message = error.to_string();
        assert!(message.contains("TOML parsing error"));
        assert!(!message.contains(" in "));
    }

    #[cfg(feature = "toml")]
    #[test]
    fn test_toml_error_conversion() {
        let toml_error = toml::from_str::<toml::Value>("not valid = [").unwrap_err();
        let error: Error = toml_error.into();

        match error {
            Error::Toml { source: _, path } => {
                assert_eq!(path, None);
            }
            _ => panic!("Expected Toml error variant"),
        }
    }

    #[cfg(feature = "toml")]
    #[test]
    fn test_toml_error_diagnostics() {
        use miette::Diagnostic;

        let toml_error = toml::from_str::<toml::Value>("not valid = [").unwrap_err();
        let error = Error::Toml {
            source: toml_error,
            path: None,
        };

        assert_eq!(
            error.code().map(|c| c.to_string()),
            Some("cuenv::workspaces::toml_error".to_string())
        );
        assert!(error.help().is_some());
    }

    #[test]
    fn test_result_type_with_question_mark() {
        fn returns_result() -> Result<String> {
            Ok("success".to_string())
        }

        fn uses_result() -> Result<String> {
            let value = returns_result()?;
            Ok(value)
        }

        assert!(uses_result().is_ok());
    }

    #[test]
    fn test_diagnostic_codes() {
        use miette::Diagnostic;

        let error = Error::WorkspaceNotFound {
            path: PathBuf::from("/test"),
        };
        assert!(error.code().is_some());

        let error = Error::InvalidWorkspaceConfig {
            path: PathBuf::from("/test"),
            message: "test".to_string(),
        };
        assert!(error.code().is_some());

        let error = Error::LockfileNotFound {
            path: PathBuf::from("/test"),
        };
        assert!(error.code().is_some());

        let error = Error::LockfileParseFailed {
            path: PathBuf::from("/test"),
            message: "test".to_string(),
        };
        assert!(error.code().is_some());

        let error = Error::MemberNotFound {
            name: "test".to_string(),
            workspace_root: PathBuf::from("/test"),
        };
        assert!(error.code().is_some());

        let error = Error::DependencyResolutionFailed {
            message: "test".to_string(),
        };
        assert!(error.code().is_some());

        let error = Error::UnsupportedPackageManager {
            manager: "test".to_string(),
        };
        assert!(error.code().is_some());
    }

    #[test]
    fn test_diagnostic_help_messages() {
        use miette::Diagnostic;

        let error = Error::WorkspaceNotFound {
            path: PathBuf::from("/test"),
        };
        assert!(error.help().is_some());

        let error = Error::InvalidWorkspaceConfig {
            path: PathBuf::from("/test"),
            message: "test".to_string(),
        };
        assert!(error.help().is_some());

        let error = Error::LockfileNotFound {
            path: PathBuf::from("/test"),
        };
        assert!(error.help().is_some());

        let error = Error::LockfileParseFailed {
            path: PathBuf::from("/test"),
            message: "test".to_string(),
        };
        assert!(error.help().is_some());
    }
}
