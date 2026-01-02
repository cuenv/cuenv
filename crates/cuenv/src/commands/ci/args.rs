//! CLI argument definitions for the CI command.

use clap::{Args, ValueEnum};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Arguments for the `cuenv ci` command.
#[derive(Debug, Clone, Args)]
pub struct CiArgs {
    /// Pipeline to run (defaults to "default").
    #[arg(long, short = 'p', value_name = "NAME")]
    pub pipeline: Option<String>,

    /// Export format instead of running.
    ///
    /// When specified, outputs pipeline YAML to stdout (or file with --output).
    /// Use for Buildkite dynamic pipeline upload or generating static files.
    #[arg(long, value_name = "FORMAT", value_enum)]
    pub export: Option<ExportFormat>,

    /// Output path for export (defaults to stdout).
    #[arg(long, short = 'o', value_name = "PATH")]
    pub output: Option<PathBuf>,

    /// Filter matrix dimensions (e.g., os=linux,arch=amd64).
    ///
    /// Run only tasks matching the specified matrix values.
    /// Useful for cross-platform CI where each runner handles one platform.
    #[arg(long, value_name = "KEY=VALUE", value_delimiter = ',')]
    pub filter_matrix: Vec<String>,

    /// Maximum parallel tasks (0 = num_cpus).
    #[arg(long, short = 'j', default_value = "0", value_name = "N")]
    pub jobs: usize,

    /// Base ref for affected task detection.
    ///
    /// Compare against this ref to determine which tasks need to run.
    /// If not specified, uses the CI provider's default (e.g., PR base branch).
    #[arg(long, value_name = "REF")]
    pub from: Option<String>,

    /// Show what would be executed without running.
    #[arg(long)]
    pub dry_run: bool,

    /// Environment for secrets resolution.
    #[arg(long, short = 'e', value_name = "NAME")]
    pub environment: Option<String>,

    /// Path to directory containing CUE files.
    #[arg(long, default_value = ".", value_name = "PATH")]
    pub path: String,

    /// Name of the CUE package to evaluate.
    #[arg(long, default_value = "cuenv", value_name = "PACKAGE")]
    pub package: String,
}

impl CiArgs {
    /// Get the effective pipeline name (defaults to "default").
    #[must_use]
    pub fn pipeline_name(&self) -> &str {
        self.pipeline.as_deref().unwrap_or("default")
    }

    /// Get the effective parallelism level.
    #[must_use]
    pub fn effective_jobs(&self) -> usize {
        if self.jobs == 0 {
            std::thread::available_parallelism()
                .map(std::num::NonZero::get)
                .unwrap_or(1)
        } else {
            self.jobs
        }
    }
}

/// Export format for CI pipeline generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExportFormat {
    /// Buildkite pipeline YAML (for dynamic upload).
    Buildkite,
    /// GitLab CI YAML.
    Gitlab,
    /// GitHub Actions workflow.
    GithubActions,
    /// CircleCI config.
    Circleci,
}

impl ExportFormat {
    /// Get the format name as a string.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Buildkite => "buildkite",
            Self::Gitlab => "gitlab",
            Self::GithubActions => "github-actions",
            Self::Circleci => "circleci",
        }
    }

    /// Get the default file extension for this format.
    #[must_use]
    pub const fn extension(&self) -> &'static str {
        match self {
            Self::Buildkite | Self::Gitlab | Self::GithubActions => "yml",
            Self::Circleci => "yml",
        }
    }
}

impl std::fmt::Display for ExportFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pipeline_name_default() {
        let args = CiArgs {
            pipeline: None,
            export: None,
            output: None,
            filter_matrix: vec![],
            jobs: 0,
            from: None,
            dry_run: false,
            environment: None,
            path: ".".to_string(),
            package: "cuenv".to_string(),
        };
        assert_eq!(args.pipeline_name(), "default");
    }

    #[test]
    fn test_pipeline_name_explicit() {
        let args = CiArgs {
            pipeline: Some("release".to_string()),
            export: None,
            output: None,
            filter_matrix: vec![],
            jobs: 0,
            from: None,
            dry_run: false,
            environment: None,
            path: ".".to_string(),
            package: "cuenv".to_string(),
        };
        assert_eq!(args.pipeline_name(), "release");
    }

    #[test]
    fn test_effective_jobs_default() {
        let args = CiArgs {
            pipeline: None,
            export: None,
            output: None,
            filter_matrix: vec![],
            jobs: 0,
            from: None,
            dry_run: false,
            environment: None,
            path: ".".to_string(),
            package: "cuenv".to_string(),
        };
        // Should return num_cpus, which is at least 1
        assert!(args.effective_jobs() >= 1);
    }

    #[test]
    fn test_effective_jobs_explicit() {
        let args = CiArgs {
            pipeline: None,
            export: None,
            output: None,
            filter_matrix: vec![],
            jobs: 8,
            from: None,
            dry_run: false,
            environment: None,
            path: ".".to_string(),
            package: "cuenv".to_string(),
        };
        assert_eq!(args.effective_jobs(), 8);
    }

    #[test]
    fn test_export_format_as_str() {
        assert_eq!(ExportFormat::Buildkite.as_str(), "buildkite");
        assert_eq!(ExportFormat::Gitlab.as_str(), "gitlab");
        assert_eq!(ExportFormat::GithubActions.as_str(), "github-actions");
        assert_eq!(ExportFormat::Circleci.as_str(), "circleci");
    }

    #[test]
    fn test_export_format_extension() {
        assert_eq!(ExportFormat::Buildkite.extension(), "yml");
        assert_eq!(ExportFormat::Gitlab.extension(), "yml");
        assert_eq!(ExportFormat::GithubActions.extension(), "yml");
        assert_eq!(ExportFormat::Circleci.extension(), "yml");
    }

    #[test]
    fn test_export_format_display() {
        assert_eq!(format!("{}", ExportFormat::Buildkite), "buildkite");
        assert_eq!(format!("{}", ExportFormat::Gitlab), "gitlab");
    }
}
