//! Session-only settings editing for terminal presentation.
//!
//! A draft is deliberately pure: applying it changes the caller-visible
//! session configuration only. Persistence remains a separate host concern.

use super::config::{ConfigError, TerminalConfig};

#[derive(Debug, Clone, PartialEq)]
pub struct SettingsDraft {
    applied: TerminalConfig,
    draft: TerminalConfig,
}

impl SettingsDraft {
    pub fn new(config: TerminalConfig) -> Self {
        debug_assert!(
            config.validate().is_ok(),
            "settings require valid configuration"
        );
        Self {
            applied: config.clone(),
            draft: config,
        }
    }

    pub fn applied(&self) -> &TerminalConfig {
        &self.applied
    }
    pub fn config(&self) -> &TerminalConfig {
        &self.draft
    }
    pub fn draft(&self) -> &TerminalConfig {
        &self.draft
    }
    pub fn is_dirty(&self) -> bool {
        self.applied != self.draft
    }

    pub fn set_font_size(&mut self, size_px: f32) -> Result<(), ConfigError> {
        self.update(|config| config.font.size_px = size_px)
    }

    pub fn set_line_height_multiplier(&mut self, multiplier: f32) -> Result<(), ConfigError> {
        self.update(|config| config.font.line_height_multiplier = multiplier)
    }

    pub fn apply(self) -> Result<TerminalConfig, ConfigError> {
        self.draft.validate()?;
        Ok(self.draft)
    }

    pub fn cancel(&mut self) {
        self.draft = self.applied.clone();
    }

    pub fn reset(&mut self) {
        self.draft = TerminalConfig::default();
    }
    pub fn reset_defaults(&mut self) {
        self.reset();
    }

    fn update(&mut self, change: impl FnOnce(&mut TerminalConfig)) -> Result<(), ConfigError> {
        let mut candidate = self.draft.clone();
        change(&mut candidate);
        candidate.validate()?;
        self.draft = candidate;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_edit_rolls_back_without_dirtying_the_draft() {
        let mut settings = SettingsDraft::new(TerminalConfig::default());
        let original = settings.draft().clone();
        assert!(matches!(
            settings.set_font_size(0.0),
            Err(ConfigError::InvalidFontSize(0.0))
        ));
        assert_eq!(settings.draft(), &original);
        assert!(!settings.is_dirty());
    }

    #[test]
    fn cancel_discards_session_only_edits() {
        let mut settings = SettingsDraft::new(TerminalConfig::default());
        settings.set_font_size(18.0).unwrap();
        assert!(settings.is_dirty());
        settings.cancel();
        assert_eq!(settings.draft(), settings.applied());
    }

    #[test]
    fn apply_commits_a_valid_draft() {
        let mut settings = SettingsDraft::new(TerminalConfig::default());
        settings.set_line_height_multiplier(1.5).unwrap();
        let applied = settings.apply().unwrap();
        assert_eq!(applied.font.line_height_multiplier, 1.5);
    }

    #[test]
    fn reset_replaces_only_the_draft() {
        let mut base = TerminalConfig::default();
        base.font.size_px = 20.0;
        let mut settings = SettingsDraft::new(base);
        settings.reset();
        assert_eq!(settings.draft().font.size_px, 16.0);
        assert_eq!(settings.applied().font.size_px, 20.0);
        assert!(settings.is_dirty());
    }
}
