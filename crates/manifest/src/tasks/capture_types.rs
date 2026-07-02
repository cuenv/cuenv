use serde::{Deserialize, Serialize};

// =============================================================================
// Task Captures (Regex Extraction from Output)
// =============================================================================

/// Source stream for regex capture extraction
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum CaptureSource {
    #[default]
    /// Capture from standard output
    Stdout,
    /// Capture from standard error
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
    /// Marker distinguishing capture references during deserialization
    pub cuenv_capture_ref: bool,
    /// Task path whose captures are referenced
    pub cuenv_task: String,
    /// Name of the referenced capture
    pub cuenv_capture: String,
}
