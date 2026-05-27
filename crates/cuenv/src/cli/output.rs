//! CLI output formats and JSON response envelopes.

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

/// Output format for command results
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, ValueEnum, Serialize, Deserialize, Default)]
#[must_use]
pub enum OutputFormat {
    /// JSON output format
    Json,
    /// Environment variable format (KEY=VALUE lines)
    Env,
    /// Plain text format (no colors or styling)
    #[default]
    Text,
    /// Rich styled output with colors and formatting
    Rich,
    /// Category-grouped tables with box-drawing characters
    Tables,
    /// Status dashboard with cache state and timing
    Dashboard,
    /// Emoji taxonomy with semantic prefixes
    Emoji,
}

impl OutputFormat {
    /// Convert a `--json` CLI flag to an `OutputFormat`.
    pub const fn from_json_flag(json: bool) -> Self {
        if json { Self::Json } else { Self::Text }
    }

    /// Check whether output should be JSON-formatted.
    #[must_use]
    pub const fn is_json(self) -> bool {
        matches!(self, Self::Json)
    }
}

impl std::fmt::Display for OutputFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            Self::Json => "json",
            Self::Env => "env",
            Self::Text => "text",
            Self::Rich => "rich",
            Self::Tables => "tables",
            Self::Dashboard => "dashboard",
            Self::Emoji => "emoji",
        };
        write!(f, "{s}")
    }
}

impl AsRef<str> for OutputFormat {
    fn as_ref(&self) -> &str {
        match self {
            Self::Json => "json",
            Self::Env => "env",
            Self::Text => "text",
            Self::Rich => "rich",
            Self::Tables => "tables",
            Self::Dashboard => "dashboard",
            Self::Emoji => "emoji",
        }
    }
}

/// Success response envelope for JSON output
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OkEnvelope<T> {
    /// Status indicator - always "ok" for success
    pub status: &'static str,
    /// The actual data payload
    pub data: T,
}

impl<T> OkEnvelope<T> {
    /// Create a new success envelope
    #[must_use]
    pub const fn new(data: T) -> Self {
        Self { status: "ok", data }
    }
}

/// Error response envelope for JSON output
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorEnvelope<E> {
    /// Status indicator - always "error" for failures
    pub status: &'static str,
    /// The error details
    pub error: E,
}

impl<E> ErrorEnvelope<E> {
    /// Create a new error envelope
    #[must_use]
    pub const fn new(error: E) -> Self {
        Self {
            status: "error",
            error,
        }
    }
}
