//! Salt configuration for secret fingerprinting with rotation support

/// Salt configuration for secret fingerprinting with rotation support
#[derive(Debug, Clone, Default)]
pub struct SaltConfig {
    /// Current salt (used for writing new fingerprints)
    pub current: Option<String>,
    /// Previous salt (accepted for reading during rotation)
    pub previous: Option<String>,
}

impl SaltConfig {
    /// Create a new salt config with only a current salt
    #[must_use]
    pub fn new(current: Option<String>) -> Self {
        Self {
            current,
            previous: None,
        }
    }

    /// Create a salt config with rotation support
    #[must_use]
    pub fn with_rotation(current: Option<String>, previous: Option<String>) -> Self {
        Self { current, previous }
    }

    /// Check if any salt is available
    #[must_use]
    pub fn has_salt(&self) -> bool {
        self.current.is_some() || self.previous.is_some()
    }

    /// Get the current salt for writing (returns current, or previous if current is None)
    #[must_use]
    pub fn write_salt(&self) -> Option<&str> {
        self.current.as_deref().or(self.previous.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_salt_config_new() {
        let config = SaltConfig::new(Some("current".to_string()));
        assert_eq!(config.current, Some("current".to_string()));
        assert_eq!(config.previous, None);
        assert!(config.has_salt());
        assert_eq!(config.write_salt(), Some("current"));
    }

    #[test]
    fn test_salt_config_with_rotation() {
        let config =
            SaltConfig::with_rotation(Some("new-salt".to_string()), Some("old-salt".to_string()));
        assert_eq!(config.current, Some("new-salt".to_string()));
        assert_eq!(config.previous, Some("old-salt".to_string()));
        assert!(config.has_salt());
        assert_eq!(config.write_salt(), Some("new-salt"));
    }

    #[test]
    fn test_salt_config_no_current_uses_previous() {
        let config = SaltConfig::with_rotation(None, Some("old-salt".to_string()));
        assert!(config.has_salt());
        assert_eq!(config.write_salt(), Some("old-salt"));
    }

    #[test]
    fn test_salt_config_empty() {
        let config = SaltConfig::default();
        assert!(!config.has_salt());
        assert_eq!(config.write_salt(), None);
    }
}
