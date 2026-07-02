//! Task retry configuration.

use serde::{Deserialize, Serialize};

/// Retry configuration for failed tasks
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RetryConfig {
    /// Number of retry attempts (default: 3)
    #[serde(default = "default_retry_attempts")]
    pub attempts: u32,
    /// Delay between retries (e.g., "5s")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delay: Option<String>,
}

fn default_retry_attempts() -> u32 {
    3
}
