//! Enhanced tracing configuration for cuenv CLI
//!
//! This module provides structured, contextual tracing with multiple output formats,
//! correlation IDs, and performance instrumentation.

use std::io;
pub use tracing::Level;
use tracing_subscriber::{filter::EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};
use uuid::Uuid;

/// Tracing output format options
#[derive(Debug, Clone, clap::ValueEnum)]
pub enum TracingFormat {
    /// Pretty-printed human-readable format
    Pretty,
    /// Compact single-line format
    Compact,
    /// Structured JSON format
    Json,
    /// Development format with extra context
    Dev,
}

/// Log level options for CLI
#[derive(Debug, Clone, clap::ValueEnum)]
pub enum LogLevel {
    /// Show all logs (trace level)
    Trace,
    /// Show debug and above
    Debug,
    /// Show info and above
    Info,
    /// Show warnings and above (default)
    Warn,
    /// Show errors only
    Error,
}

impl From<LogLevel> for Level {
    fn from(level: LogLevel) -> Self {
        match level {
            LogLevel::Trace => Level::TRACE,
            LogLevel::Debug => Level::DEBUG,
            LogLevel::Info => Level::INFO,
            LogLevel::Warn => Level::WARN,
            LogLevel::Error => Level::ERROR,
        }
    }
}

impl std::str::FromStr for TracingFormat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "pretty" => Ok(TracingFormat::Pretty),
            "compact" => Ok(TracingFormat::Compact),
            "json" => Ok(TracingFormat::Json),
            "dev" => Ok(TracingFormat::Dev),
            _ => Err(format!("Unknown tracing format: {s}")),
        }
    }
}

/// Tracing configuration
#[derive(Debug, Clone)]
pub struct TracingConfig {
    pub format: TracingFormat,
    pub level: Level,
    pub enable_correlation_ids: bool,
    pub enable_timestamps: bool,
    pub enable_file_location: bool,
    pub filter: Option<String>,
}

impl Default for TracingConfig {
    fn default() -> Self {
        Self {
            format: TracingFormat::Pretty,
            level: Level::WARN, // Default to quiet operation
            enable_correlation_ids: true,
            enable_timestamps: true,
            enable_file_location: true,
            filter: None,
        }
    }
}

/// Global correlation ID for tracing request correlation
static CORRELATION_ID: std::sync::OnceLock<Uuid> = std::sync::OnceLock::new();

/// Get or create a correlation ID for the current session
pub fn correlation_id() -> Uuid {
    *CORRELATION_ID.get_or_init(Uuid::new_v4)
}

/// Initialize tracing with the given configuration
pub fn init_tracing(config: TracingConfig) -> miette::Result<()> {
    let correlation_id = correlation_id();

    // Create base filter
    let env_filter = if let Some(filter) = config.filter {
        EnvFilter::try_new(filter)
    } else {
        EnvFilter::try_from_default_env().or_else(|_| {
            let level_str = match config.level {
                Level::TRACE => "trace",
                Level::DEBUG => "debug",
                Level::INFO => "info",
                Level::WARN => "warn",
                Level::ERROR => "error",
            };
            EnvFilter::try_new(format!(
                "cuenv={level_str},cuenv_cli={level_str},cuenv_core={level_str},cuengine={level_str}"
            ))
        })
    }
    .map_err(|e| miette::miette!("Failed to create tracing filter: {e}"))?;

    let registry = tracing_subscriber::registry().with(env_filter);

    match config.format {
        TracingFormat::Pretty => {
            let layer = tracing_subscriber::fmt::layer()
                .pretty()
                .with_writer(io::stderr)
                .with_target(true)
                .with_thread_ids(true)
                .with_thread_names(true);

            registry.with(layer).init();
        }
        TracingFormat::Compact => {
            let layer = tracing_subscriber::fmt::layer()
                .compact()
                .with_writer(io::stderr)
                .with_target(false)
                .with_thread_ids(false);

            registry.with(layer).init();
        }
        TracingFormat::Json => {
            let layer = tracing_subscriber::fmt::layer()
                .json()
                .with_writer(io::stderr)
                .with_current_span(true)
                .with_span_list(true);

            registry.with(layer).init();
        }
        TracingFormat::Dev => {
            let layer = tracing_subscriber::fmt::layer()
                .with_writer(io::stderr)
                .with_file(config.enable_file_location)
                .with_line_number(config.enable_file_location)
                .with_target(true)
                .with_thread_ids(true)
                .with_thread_names(true)
                .with_level(true);

            registry.with(layer).init();
        }
    }

    tracing::info!(
        correlation_id = %correlation_id,
        version = env!("CARGO_PKG_VERSION"),
        format = ?config.format,
        "Tracing initialized for cuenv CLI"
    );

    Ok(())
}

/// Create a new span for command execution with structured fields
#[macro_export]
macro_rules! command_span {
    ($command:expr) => {
        tracing::info_span!(
            "command",
            command = %$command,
            correlation_id = %$crate::tracing::correlation_id(),
            start_time = %chrono::Utc::now().to_rfc3339(),
        )
    };
    ($command:expr, $($key:expr => $value:expr),+ $(,)?) => {
        tracing::info_span!(
            "command",
            command = %$command,
            correlation_id = %$crate::tracing::correlation_id(),
            start_time = %chrono::Utc::now().to_rfc3339(),
            $($key = $value),+
        )
    };
}

/// Instrument a function with tracing span
pub use tracing::instrument;

/// Create performance measurement span
#[macro_export]
macro_rules! perf_span {
    ($name:expr) => {
        tracing::debug_span!(
            "perf",
            operation = %$name,
            correlation_id = %$crate::tracing::correlation_id(),
        )
    };
}

/// Log performance metrics
#[macro_export]
macro_rules! perf_event {
    ($operation:expr, $duration:expr) => {
        tracing::debug!(
            operation = %$operation,
            duration_ms = $duration.as_millis(),
            correlation_id = %$crate::tracing::correlation_id(),
            "Performance measurement"
        );
    };
    ($operation:expr, $duration:expr, $($key:expr => $value:expr),+ $(,)?) => {
        tracing::debug!(
            operation = %$operation,
            duration_ms = $duration.as_millis(),
            correlation_id = %$crate::tracing::correlation_id(),
            $($key = $value),+
            "Performance measurement"
        );
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_parsing() {
        assert!(matches!(
            "pretty".parse::<TracingFormat>().unwrap(),
            TracingFormat::Pretty
        ));
        assert!(matches!(
            "json".parse::<TracingFormat>().unwrap(),
            TracingFormat::Json
        ));
        assert!("invalid".parse::<TracingFormat>().is_err());
    }

    #[test]
    fn test_correlation_id_consistency() {
        let id1 = correlation_id();
        let id2 = correlation_id();
        assert_eq!(id1, id2);
    }
}
