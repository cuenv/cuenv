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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ci_context_default() {
        let ctx = CIContext::default();
        assert_eq!(ctx.provider, "local");
        assert_eq!(ctx.event, "manual");
        assert!(ctx.ref_name.is_empty());
        assert!(ctx.base_ref.is_none());
        assert!(ctx.sha.is_empty());
    }

    #[test]
    fn test_ci_context_display_with_long_sha() {
        let ctx = CIContext {
            provider: "github".to_string(),
            event: "push".to_string(),
            ref_name: "refs/heads/main".to_string(),
            base_ref: None,
            sha: "abc1234567890def".to_string(),
        };
        let display = format!("{ctx}");
        assert!(display.contains("github/push"));
        assert!(display.contains("refs/heads/main"));
        assert!(display.contains("abc1234")); // First 7 chars
        assert!(!display.contains("567890def")); // Not the rest
    }

    #[test]
    fn test_ci_context_display_with_short_sha() {
        let ctx = CIContext {
            provider: "gitlab".to_string(),
            event: "merge_request".to_string(),
            ref_name: "refs/heads/feature".to_string(),
            base_ref: Some("refs/heads/main".to_string()),
            sha: "abc".to_string(),
        };
        let display = format!("{ctx}");
        assert!(display.contains("gitlab/merge_request"));
        assert!(display.contains("abc")); // Full short sha
    }

    #[test]
    fn test_ci_context_equality() {
        let ctx1 = CIContext {
            provider: "github".to_string(),
            event: "push".to_string(),
            ref_name: "refs/heads/main".to_string(),
            base_ref: None,
            sha: "abc123".to_string(),
        };
        let ctx2 = ctx1.clone();
        assert_eq!(ctx1, ctx2);
    }

    #[test]
    fn test_ci_context_inequality() {
        let ctx1 = CIContext::default();
        let ctx2 = CIContext {
            provider: "github".to_string(),
            ..CIContext::default()
        };
        assert_ne!(ctx1, ctx2);
    }

    #[test]
    fn test_ci_context_debug() {
        let ctx = CIContext::default();
        let debug_str = format!("{ctx:?}");
        assert!(debug_str.contains("CIContext"));
        assert!(debug_str.contains("local"));
    }

    #[test]
    fn test_ci_context_with_base_ref() {
        let ctx = CIContext {
            provider: "github".to_string(),
            event: "pull_request".to_string(),
            ref_name: "refs/pull/123/head".to_string(),
            base_ref: Some("refs/heads/main".to_string()),
            sha: "abc1234".to_string(),
        };
        assert_eq!(ctx.base_ref, Some("refs/heads/main".to_string()));
    }
}
