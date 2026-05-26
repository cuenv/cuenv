use serde::{Deserialize, Serialize};

// =============================================================================
// Task Captures (Regex Extraction from Output)
// =============================================================================

/// Source stream for regex capture extraction
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum CaptureSource {
    #[default]
    Stdout,
    Stderr,
}

/// Regex capture definition for extracting values from task output
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TaskCapture {
    /// Regex pattern with capture group - first group's match becomes the value
    pub pattern: String,
    /// Which output stream to search (default: stdout)
    #[serde(default)]
    pub source: CaptureSource,
}

/// Reference to a captured value, resolved at runtime
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TaskCaptureRef {
    pub cuenv_capture_ref: bool,
    pub cuenv_task: String,
    pub cuenv_capture: String,
}
