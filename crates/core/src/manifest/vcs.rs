use serde::{Deserialize, Serialize};

/// A cuenv-managed Git dependency.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct VcsDependency {
    /// Git repository URL.
    pub url: String,
    /// Branch, tag, or commit-ish to resolve.
    #[serde(default = "default_vcs_reference")]
    pub reference: String,
    /// Whether to materialize a tracked source snapshot.
    pub vendor: bool,
    /// Repository-relative materialization path.
    pub path: String,
    /// Subdirectory of the repo to materialize via sparse checkout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subdir: Option<String>,
}

fn default_vcs_reference() -> String {
    "HEAD".to_string()
}
