//! Enhanced error display using miette for beautiful, contextual error reporting
//!
//! This module provides CLI-specific error types and display formatting
//! that leverages miette's diagnostic capabilities for improved user experience.

use miette::{Diagnostic, SourceSpan};
use thiserror::Error;

/// CLI-specific error types with enhanced diagnostics
#[derive(Error, Debug, Diagnostic)]
#[allow(dead_code)]
pub enum CliError {
    #[error("Command execution failed")]
    #[diagnostic(code(cuenv::cli::command_failed))]
    CommandFailed {
        command: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
        suggestions: Option<Vec<String>>,
    },

    #[error("Configuration parsing failed")]
    #[diagnostic(code(cuenv::cli::config_parse_error))]
    ConfigParseError {
        config_file: String,
        #[source_code]
        src: String,
        #[label("error occurred here")]
        error_span: SourceSpan,
        #[help]
        help_text: Option<String>,
    },

    #[error("Invalid command line argument")]
    #[diagnostic(
        code(cuenv::cli::invalid_argument),
        help("Run 'cuenv --help' to see available options")
    )]
    InvalidArgument {
        argument: String,
        expected_values: Option<Vec<String>>,
        suggestion: Option<String>,
    },

    #[error("File operation failed")]
    #[diagnostic(
        code(cuenv::cli::file_error),
        help("Check file permissions and ensure the path exists")
    )]
    FileError {
        operation: String,
        path: std::path::PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("Tracing initialization failed")]
    #[diagnostic(
        code(cuenv::cli::tracing_error),
        help("Check your tracing configuration and environment variables")
    )]
    TracingError {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
        config_used: String,
    },
}

#[allow(dead_code)]
impl CliError {
    pub fn command_failed(
        command: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        CliError::CommandFailed {
            command: command.into(),
            source: Box::new(source),
            suggestions: None,
        }
    }

    pub fn command_failed_with_suggestions(
        command: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
        suggestions: Vec<String>,
    ) -> Self {
        CliError::CommandFailed {
            command: command.into(),
            source: Box::new(source),
            suggestions: Some(suggestions),
        }
    }

    pub fn config_parse_error(
        config_file: impl Into<String>,
        src: impl Into<String>,
        error_span: SourceSpan,
    ) -> Self {
        CliError::ConfigParseError {
            config_file: config_file.into(),
            src: src.into(),
            error_span,
            help_text: None,
        }
    }

    pub fn invalid_argument(argument: impl Into<String>) -> Self {
        CliError::InvalidArgument {
            argument: argument.into(),
            expected_values: None,
            suggestion: None,
        }
    }

    pub fn invalid_argument_with_values(
        argument: impl Into<String>,
        expected_values: Vec<String>,
    ) -> Self {
        CliError::InvalidArgument {
            argument: argument.into(),
            expected_values: Some(expected_values),
            suggestion: None,
        }
    }

    pub fn file_error(
        operation: impl Into<String>,
        path: impl Into<std::path::PathBuf>,
        source: std::io::Error,
    ) -> Self {
        CliError::FileError {
            operation: operation.into(),
            path: path.into(),
            source,
        }
    }
}

/// Enhanced error reporter with custom formatting
#[allow(dead_code)]
pub struct ErrorReporter {
    use_colors: bool,
    show_source_code: bool,
    show_help: bool,
}

impl Default for ErrorReporter {
    fn default() -> Self {
        Self {
            use_colors: true,
            show_source_code: true,
            show_help: true,
        }
    }
}

#[allow(dead_code)]
impl ErrorReporter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_colors(mut self, use_colors: bool) -> Self {
        self.use_colors = use_colors;
        self
    }

    pub fn with_source_code(mut self, show_source_code: bool) -> Self {
        self.show_source_code = show_source_code;
        self
    }

    pub fn with_help(mut self, show_help: bool) -> Self {
        self.show_help = show_help;
        self
    }

    /// Report an error using miette's fancy formatting
    pub fn report(&self, error: &dyn Diagnostic) -> miette::Result<()> {
        // Use miette's default error reporting
        eprintln!("{error:?}");

        // TODO: add proper tracing when fixed

        Ok(())
    }

    /// Handle and report any error, converting it to a diagnostic if needed
    pub fn handle_error(&self, error: &dyn Diagnostic) -> miette::Result<()> {
        self.report(error)?;
        Ok(())
    }
}

/// Convenience function to report errors with default settings
pub fn _report_error(error: &dyn Diagnostic) {
    let reporter = ErrorReporter::default();

    if let Err(report_err) = reporter.handle_error(error) {
        eprintln!("Failed to report error: {report_err}");
        eprintln!("Original error: {error}");
    }
}

/// Enhanced result type that automatically handles error reporting
#[allow(dead_code)]
pub type CliResult<T> = Result<T, CliError>;

/// Extension trait for Result types to enable easy error reporting
pub trait _ResultExt<T> {
    fn report_on_error(self) -> Self;
    fn report_and_exit(self, exit_code: i32) -> T;
}

impl<T, E> _ResultExt<T> for Result<T, E>
where
    E: std::error::Error + Diagnostic + 'static,
{
    fn report_on_error(self) -> Self {
        if let Err(ref error) = self {
            _report_error(error);
        }
        self
    }

    fn report_and_exit(self, exit_code: i32) -> T {
        match self {
            Ok(value) => value,
            Err(error) => {
                _report_error(&error);
                std::process::exit(exit_code);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use miette::SourceSpan;

    #[test]
    fn test_cli_error_creation() {
        let error = CliError::invalid_argument("--invalid-flag");
        assert!(error.to_string().contains("Invalid command line argument"));
    }

    #[test]
    fn test_config_parse_error() {
        let source = "field: invalid value\n";
        let error = CliError::config_parse_error(
            "cuenv.cue",
            source,
            SourceSpan::new(7_usize.into(), 13_usize),
        );

        assert!(error.to_string().contains("Configuration parsing failed"));
    }

    #[test]
    fn test_error_reporter() {
        let reporter = ErrorReporter::new().with_colors(false).with_help(true);

        let error = CliError::invalid_argument("--test");
        // Test doesn't fail if reporter works correctly
        let _ = reporter.report(&error);
    }
}
