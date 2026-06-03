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
    /// Overlay mode: materialize each immediate child of the subtree into its own
    /// `path/<child>` and gitignore each child individually, leaving the parent
    /// `path` un-ignored and never replaced wholesale. Requires `subdir` and
    /// `vendor: false`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub overlay: bool,
}

fn default_vcs_reference() -> String {
    "HEAD".to_string()
}
