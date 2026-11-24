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

    /// Run every task in the selected pipelines without affected-task filtering.
    #[arg(long, conflicts_with = "export")]
    pub all: bool,

    /// Export format instead of running.
    ///
    /// When specified, outputs pipeline YAML to stdout (or file with --output).
    /// Use for Buildkite dynamic pipeline upload or generating static files.
    #[arg(long, value_name = "FORMAT", value_enum)]
    pub export: Option<ExportFormat>,

    /// Output path for export or for the `--plan-format` plan (defaults to stdout).
    #[arg(long, short = 'o', value_name = "PATH")]
    pub output: Option<PathBuf>,

    /// Maximum parallel task DAG jobs (0 = host parallelism).
    #[arg(long, short = 'j', default_value = "0", value_name = "N")]
    pub jobs: usize,

    /// Base ref for affected task detection.
    ///
    /// `HEAD` is compared with its merge base with this ref to determine
    /// which tasks need to run. Ignored when GitHub Actions or Buildkite is
    /// detected: the provider supplies the base there. Without it, a local
    /// run sees uncommitted changes only, and a run on a CI system cuenv
    /// does not recognise runs the whole pipeline.
    #[arg(long, value_name = "REF")]
    pub from: Option<String>,

    /// Show what would be executed without running.
    #[arg(long)]
    pub dry_run: bool,

    /// Write the tasks the dry run selected in machine-readable form.
    ///
    /// `lines` writes the selected task names, sorted, one per line (nothing
    /// selected is empty output); `json` writes the selection per project.
    /// Use `--output` to write to a file: stdout also carries the run's own
    /// messages.
    #[arg(
        long,
        value_name = "FORMAT",
        value_enum,
        requires = "dry_run",
        conflicts_with = "export"
    )]
    pub plan_format: Option<PlanFormatArg>,

    /// Environment for secrets resolution.
    #[arg(long, short = 'e', value_name = "NAME")]
    pub environment: Option<String>,

    /// Path to directory containing CUE files.
    #[arg(long, default_value = ".", value_name = "PATH")]
    pub path: String,

    /// Name of the CUE package to evaluate.
    #[arg(long, default_value = "cuenv", value_name = "PACKAGE")]
    pub package: String,

    /// Deterministic task shard in `<INDEX>/<TOTAL>` format (1-indexed, e.g. `1/4`).
    #[arg(long, value_name = "INDEX/TOTAL", conflicts_with = "export")]
    pub shard: Option<cuenv_ci::executor::ShardSpec>,
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
            std::thread::available_parallelism().map_or(1, std::num::NonZero::get)
        } else {
            self.jobs
        }
    }
}

/// Machine-readable format of a dry run's plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PlanFormatArg {
    /// One selected task name per line.
    Lines,
    /// A JSON document with the selection per project.
    Json,
}

impl From<PlanFormatArg> for cuenv_ci::executor::PlanFormat {
    fn from(format: PlanFormatArg) -> Self {
        match format {
            PlanFormatArg::Lines => Self::Lines,
            PlanFormatArg::Json => Self::Json,
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
            Self::Buildkite | Self::Gitlab | Self::GithubActions | Self::Circleci => "yml",
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

    #[derive(clap::Parser)]
    struct ParseCiArgs {
        #[command(flatten)]
        ci: CiArgs,
    }

    #[test]
    fn all_flag_is_opt_in_and_conflicts_with_export() {
        use clap::Parser;
        assert!(!ParseCiArgs::try_parse_from(["cuenv"]).unwrap().ci.all);
        assert!(
            ParseCiArgs::try_parse_from(["cuenv", "--all", "--pipeline", "verify"])
                .unwrap()
                .ci
                .all
        );
        assert!(
            ParseCiArgs::try_parse_from(["cuenv", "--all", "--export", "github-actions"]).is_err()
        );
    }

    #[test]
    fn plan_format_needs_a_dry_run() {
        use clap::Parser;
        assert!(ParseCiArgs::try_parse_from(["cuenv", "--plan-format", "lines"]).is_err());
        let args =
            ParseCiArgs::try_parse_from(["cuenv", "--dry-run", "--plan-format", "lines"]).unwrap();
        assert_eq!(args.ci.plan_format, Some(PlanFormatArg::Lines));
    }

    #[test]
    fn test_pipeline_name_default() {
        let args = CiArgs {
            pipeline: None,
            all: false,
            export: None,
            output: None,
            jobs: 0,
            from: None,
            dry_run: false,
            plan_format: None,
            environment: None,
            path: ".".to_string(),
            package: "cuenv".to_string(),
            shard: None,
        };
        assert_eq!(args.pipeline_name(), "default");
    }

    #[test]
    fn test_pipeline_name_explicit() {
        let args = CiArgs {
            pipeline: Some("release".to_string()),
            all: false,
            export: None,
            output: None,
            jobs: 0,
            from: None,
            dry_run: false,
            plan_format: None,
            environment: None,
            path: ".".to_string(),
            package: "cuenv".to_string(),
            shard: None,
        };
        assert_eq!(args.pipeline_name(), "release");
    }

    #[test]
    fn test_effective_jobs_default() {
        let args = CiArgs {
            pipeline: None,
            all: false,
            export: None,
            output: None,
            jobs: 0,
            from: None,
            dry_run: false,
            plan_format: None,
            environment: None,
            path: ".".to_string(),
            package: "cuenv".to_string(),
            shard: None,
        };
        // Should return num_cpus, which is at least 1
        assert!(args.effective_jobs() >= 1);
    }

    #[test]
    fn test_effective_jobs_explicit() {
        let args = CiArgs {
            pipeline: None,
            all: false,
            export: None,
            output: None,
            jobs: 8,
            from: None,
            dry_run: false,
            plan_format: None,
            environment: None,
            path: ".".to_string(),
            package: "cuenv".to_string(),
            shard: None,
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
