//! Metadata management for cuenv events.
//!
//! Provides correlation ID tracking and metadata context for event emission.

use std::sync::OnceLock;
use uuid::Uuid;

/// Global correlation ID for the current session.
static CORRELATION_ID: OnceLock<Uuid> = OnceLock::new();

/// Get or create a correlation ID for the current session.
///
/// This returns the same ID throughout the lifetime of the process,
/// allowing all events to be correlated together.
#[must_use]
pub fn correlation_id() -> Uuid {
    *CORRELATION_ID.get_or_init(Uuid::new_v4)
}

/// Set the correlation ID for the current session.
///
/// This can only be called once; subsequent calls will be ignored.
/// Returns `true` if the ID was set, `false` if it was already set.
pub fn set_correlation_id(id: Uuid) -> bool {
    CORRELATION_ID.set(id).is_ok()
}

/// Metadata context for event emission.
///
/// Holds configuration and state used when creating events.
#[derive(Debug, Clone)]
pub struct MetadataContext {
    /// The correlation ID for this context.
    pub correlation_id: Uuid,
    /// Optional default target for events.
    pub default_target: Option<String>,
}

impl MetadataContext {
    /// Create a new metadata context with the global correlation ID.
    #[must_use]
    pub fn new() -> Self {
        Self {
            correlation_id: correlation_id(),
            default_target: None,
        }
    }

    /// Create a new metadata context with a specific correlation ID.
    #[must_use]
    pub const fn with_correlation_id(id: Uuid) -> Self {
        Self {
            correlation_id: id,
            default_target: None,
        }
    }

    /// Set a default target for events created with this context.
    #[must_use]
    pub fn with_target(mut self, target: impl Into<String>) -> Self {
        self.default_target = Some(target.into());
        self
    }
}

impl Default for MetadataContext {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_correlation_id_consistency() {
        let id1 = correlation_id();
        let id2 = correlation_id();
        assert_eq!(id1, id2);
    }

    #[test]
    fn test_metadata_context_creation() {
        let ctx = MetadataContext::new();
        assert!(!ctx.correlation_id.is_nil());
        assert!(ctx.default_target.is_none());
    }

    #[test]
    fn test_metadata_context_with_target() {
        let ctx = MetadataContext::new().with_target("cuenv::test");
        assert_eq!(ctx.default_target, Some("cuenv::test".to_string()));
    }

    #[test]
    fn test_metadata_context_default() {
        let ctx = MetadataContext::default();
        assert!(!ctx.correlation_id.is_nil());
        assert!(ctx.default_target.is_none());
    }

    #[test]
    fn test_metadata_context_with_correlation_id() {
        let id = Uuid::new_v4();
        let ctx = MetadataContext::with_correlation_id(id);
        assert_eq!(ctx.correlation_id, id);
        assert!(ctx.default_target.is_none());
    }

    #[test]
    fn test_metadata_context_debug() {
        let ctx = MetadataContext::new();
        let debug = format!("{:?}", ctx);
        assert!(debug.contains("MetadataContext"));
        assert!(debug.contains("correlation_id"));
    }

    #[test]
    fn test_metadata_context_clone() {
        let ctx = MetadataContext::new().with_target("test");
        let cloned = ctx.clone();
        assert_eq!(ctx.correlation_id, cloned.correlation_id);
        assert_eq!(ctx.default_target, cloned.default_target);
    }

    #[test]
    fn test_metadata_context_with_string_target() {
        let ctx = MetadataContext::new().with_target(String::from("owned-target"));
        assert_eq!(ctx.default_target, Some("owned-target".to_string()));
    }

    #[test]
    fn test_set_correlation_id_after_init() {
        // After correlation_id() has been called, set_correlation_id should return false
        let _ = correlation_id(); // Ensure it's initialized
        let new_id = Uuid::new_v4();
        let result = set_correlation_id(new_id);
        // Since we've already called correlation_id(), this should return false
        assert!(!result);
    }
}
