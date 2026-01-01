use std::fmt;

/// Context information about the current CI environment.
///
/// Contains metadata about the CI provider, event type, and git references
/// that triggered the pipeline execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CIContext {
    /// The CI provider name (e.g., "github", "gitlab", "local").
    pub provider: String,
    /// The event that triggered the pipeline (e.g., "push", "pull_request").
    pub event: String,
    /// The git ref name (e.g., "refs/heads/main", "refs/tags/v1.0.0").
    pub ref_name: String,
    /// The base ref for pull requests (e.g., "refs/heads/main").
    pub base_ref: Option<String>,
    /// The git commit SHA.
    pub sha: String,
}

impl Default for CIContext {
    fn default() -> Self {
        Self {
            provider: String::from("local"),
            event: String::from("manual"),
            ref_name: String::new(),
            base_ref: None,
            sha: String::new(),
        }
    }
}

impl fmt::Display for CIContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}/{} on {} ({})",
            self.provider,
            self.event,
            self.ref_name,
            &self.sha.get(..7).unwrap_or(&self.sha)
        )
    }
}
