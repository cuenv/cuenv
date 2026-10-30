//! The last writer: the CUE instance that last took a tenant's lock to
//! change its state.
//!
//! The record is information only. It never refuses a run: a tenant is
//! identified by its key, and project names are unique, so a second instance
//! cannot claim the same tenant. Plan shows it, and rename detection uses its
//! directory to match a missing tenant to the identity it was renamed from.

use crate::tenant::ProjectInstance;

/// The CUE instance that last acquired a tenant's lock to change its state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastWriter {
    /// The instance (`<directory>:<package>`). Read back from the store, so
    /// it is untrusted text with control characters removed.
    pub instance: ProjectInstance,
    /// When it was recorded (RFC 3339).
    pub recorded_at: String,
}

impl LastWriter {
    /// The instance's directory relative to the module root (`.` for the
    /// root).
    #[must_use]
    pub fn directory(&self) -> &str {
        self.instance
            .as_str()
            .rsplit_once(':')
            .map_or(self.instance.as_str(), |(directory, _)| directory)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_directory_is_everything_before_the_package() {
        let writer = LastWriter {
            instance: ProjectInstance::try_new("services/web", "cuenv").unwrap(),
            recorded_at: "2026-01-01T00:00:00Z".into(),
        };
        assert_eq!(writer.directory(), "services/web");
        let root = LastWriter {
            instance: ProjectInstance::try_new("", "cuenv").unwrap(),
            recorded_at: "2026-01-01T00:00:00Z".into(),
        };
        assert_eq!(root.directory(), ".");
    }
}
