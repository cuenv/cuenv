//! Renderer-independent terminal configuration.
//!
//! This module deliberately has no persistence dependency. [`ConfigStore`] and
//! [`ConfigCodec`] are the seams for a later package to add serde/file
//! persistence without coupling the terminal contract to a format today.

use std::collections::BTreeMap;
use std::fmt;

pub const CURRENT_CONFIG_VERSION: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ConfigVersion(pub u16);

impl ConfigVersion {
    pub const CURRENT: Self = Self(CURRENT_CONFIG_VERSION);

    pub const fn is_compatible(self) -> bool {
        self.0 == CURRENT_CONFIG_VERSION
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

#[derive(Debug, Clone, PartialEq)]
pub struct FontConfig {
    pub family: String,
    pub fallbacks: Vec<String>,
    pub size_px: f32,
    pub line_height_multiplier: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorPreference {
    Block,
    Bar,
    Underline,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeColors {
    pub host_background: Rgb,
    pub surface: Rgb,
    pub title_surface: Rgb,
    pub title_text: Rgb,
    pub text: Rgb,
    pub cursor: Rgb,
    pub cursor_text: Rgb,
    pub ansi: [Rgb; 16],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractionConfig {
    pub scrollback_limit: usize,
    pub selection_enabled: bool,
    pub search_enabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeybindingProfile {
    Default,
    Emacs,
    Vim,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeybindingConfig {
    pub profile: KeybindingProfile,
    /// Action names and physical key descriptions are intentionally opaque to
    /// the terminal engine; a host can replace this map deterministically.
    pub bindings: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TerminalConfig {
    pub version: ConfigVersion,
    pub font: FontConfig,
    /// Percentage of the terminal canvas background that remains transparent.
    /// Zero is fully opaque and 100 is fully transparent.
    pub terminal_transparency_percent: u8,
    pub cursor: CursorPreference,
    pub theme: ThemeColors,
    pub interaction: InteractionConfig,
    pub keybindings: KeybindingConfig,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ConfigError {
    UnsupportedVersion(ConfigVersion),
    EmptyFontFamily,
    InvalidFontSize(f32),
    InvalidLineHeight(f32),
    InvalidTerminalTransparency(u8),
    InvalidScrollback,
    EmptyKeybindingAction,
    EmptyKeybinding,
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion(version) => {
                write!(f, "unsupported config version {}", version.0)
            }
            Self::EmptyFontFamily => f.write_str("font family cannot be empty"),
            Self::InvalidFontSize(value) => {
                write!(f, "font size must be between 1 and 128px, got {value}")
            }
            Self::InvalidLineHeight(value) => write!(
                f,
                "line-height multiplier must be between 0.5 and 3, got {value}"
            ),
            Self::InvalidTerminalTransparency(value) => write!(
                f,
                "terminal transparency must be between 0 and 100%, got {value}%"
            ),
            Self::InvalidScrollback => f.write_str("scrollback limit must be non-zero"),
            Self::EmptyKeybindingAction => f.write_str("keybinding action cannot be empty"),
            Self::EmptyKeybinding => f.write_str("keybinding cannot be empty"),
        }
    }
}

impl std::error::Error for ConfigError {}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            version: ConfigVersion::CURRENT,
            font: FontConfig {
                family: "MonaspiceNe Nerd Font".into(),
                fallbacks: [
                    "Noto Color Emoji",
                    "Monaspace Neon",
                    "SF Mono",
                    "Menlo",
                    "Monaco",
                    "Apple Symbols",
                ]
                .into_iter()
                .map(String::from)
                .collect(),
                size_px: 16.0,
                line_height_multiplier: 1.2,
            },
            terminal_transparency_percent: 0,
            cursor: CursorPreference::Block,
            theme: ThemeColors::default(),
            interaction: InteractionConfig {
                scrollback_limit: 10_000,
                selection_enabled: true,
                search_enabled: true,
            },
            keybindings: KeybindingConfig::default(),
        }
    }
}

impl Default for ThemeColors {
    fn default() -> Self {
        Self::catppuccin_mocha()
    }
}

impl ThemeColors {
    /// Catppuccin Mocha is Cuetty's built-in semantic terminal palette.
    ///
    /// The first eight entries are the standard ANSI colours and the final
    /// eight are their bright variants. Keeping the palette here, next to the
    /// renderer-independent config, means Rio and every host renderer resolve
    /// the same defaults.
    pub fn catppuccin_mocha() -> Self {
        Self {
            // Crust and mantle frame the app chrome; base fills the terminal.
            host_background: Rgb(17, 17, 27),
            surface: Rgb(30, 30, 46),
            title_surface: Rgb(24, 24, 37),
            title_text: Rgb(186, 194, 222),
            text: Rgb(205, 214, 244),
            cursor: Rgb(245, 224, 230),
            cursor_text: Rgb(30, 30, 46),
            ansi: [
                Rgb(69, 71, 90),
                Rgb(243, 139, 168),
                Rgb(166, 227, 161),
                Rgb(249, 226, 175),
                Rgb(137, 180, 250),
                Rgb(245, 194, 231),
                Rgb(148, 226, 213),
                Rgb(186, 194, 222),
                Rgb(88, 91, 112),
                Rgb(243, 139, 168),
                Rgb(166, 227, 161),
                Rgb(249, 226, 175),
                Rgb(137, 180, 250),
                Rgb(245, 194, 231),
                Rgb(148, 226, 213),
                Rgb(166, 173, 200),
            ],
        }
    }
}

impl Default for KeybindingConfig {
    fn default() -> Self {
        Self {
            profile: KeybindingProfile::Default,
            bindings: BTreeMap::new(),
        }
    }
}

impl TerminalConfig {
    pub fn validate(&self) -> Result<(), ConfigError> {
        if !self.version.is_compatible() {
            return Err(ConfigError::UnsupportedVersion(self.version));
        }
        if self.font.family.trim().is_empty() {
            return Err(ConfigError::EmptyFontFamily);
        }
        if !self.font.size_px.is_finite() || !(1.0..=128.0).contains(&self.font.size_px) {
            return Err(ConfigError::InvalidFontSize(self.font.size_px));
        }
        if !self.font.line_height_multiplier.is_finite()
            || !(0.5..=3.0).contains(&self.font.line_height_multiplier)
        {
            return Err(ConfigError::InvalidLineHeight(
                self.font.line_height_multiplier,
            ));
        }
        if self.terminal_transparency_percent > 100 {
            return Err(ConfigError::InvalidTerminalTransparency(
                self.terminal_transparency_percent,
            ));
        }
        if self.interaction.scrollback_limit == 0 {
            return Err(ConfigError::InvalidScrollback);
        }
        for (action, key) in &self.keybindings.bindings {
            if action.trim().is_empty() {
                return Err(ConfigError::EmptyKeybindingAction);
            }
            if key.trim().is_empty() {
                return Err(ConfigError::EmptyKeybinding);
            }
        }
        Ok(())
    }

    pub fn with_theme(mut self, theme: ThemeColors) -> Self {
        self.theme = theme;
        self
    }
    pub fn with_keybindings(mut self, keybindings: KeybindingConfig) -> Self {
        self.keybindings = keybindings;
        self
    }
}

/// Persistence is intentionally supplied by a host package later.
pub trait ConfigStore {
    fn load(&self) -> Result<Option<TerminalConfig>, Box<dyn std::error::Error + Send + Sync>>;
    fn save(&self, config: &TerminalConfig)
    -> Result<(), Box<dyn std::error::Error + Send + Sync>>;
}
pub trait ConfigCodec {
    type Encoded;
    fn encode(&self, config: &TerminalConfig) -> Result<Self::Encoded, ConfigError>;
    fn decode(&self, encoded: &Self::Encoded) -> Result<TerminalConfig, ConfigError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_terminal_contract() {
        let config = TerminalConfig::default();
        assert_eq!(config.font.family, "MonaspiceNe Nerd Font");
        assert_eq!(
            config.font.fallbacks,
            [
                "Noto Color Emoji",
                "Monaspace Neon",
                "SF Mono",
                "Menlo",
                "Monaco",
                "Apple Symbols"
            ]
        );
        assert_eq!(config.font.size_px, 16.0);
        assert_eq!(config.font.line_height_multiplier, 1.2);
        assert_eq!(config.terminal_transparency_percent, 0);
        assert_eq!(config.theme.host_background, Rgb(17, 17, 27));
        assert_eq!(config.theme.surface, Rgb(30, 30, 46));
        assert_eq!(config.theme.title_surface, Rgb(24, 24, 37));
        assert_eq!(config.theme.text, Rgb(205, 214, 244));
        assert_eq!(config.theme.ansi[1], Rgb(243, 139, 168));
        assert_eq!(config.theme.ansi[4], Rgb(137, 180, 250));
        assert_eq!(config.theme.ansi[5], Rgb(245, 194, 231));
        assert!(config.validate().is_ok());
    }

    #[test]
    fn invalid_metrics_are_rejected() {
        let mut config = TerminalConfig::default();
        config.font.size_px = f32::NAN;
        assert!(matches!(
            config.validate(),
            Err(ConfigError::InvalidFontSize(_))
        ));
        let mut config = TerminalConfig::default();
        config.font.line_height_multiplier = 0.1;
        assert!(matches!(
            config.validate(),
            Err(ConfigError::InvalidLineHeight(_))
        ));
        let config = TerminalConfig {
            terminal_transparency_percent: 101,
            ..TerminalConfig::default()
        };
        assert!(matches!(
            config.validate(),
            Err(ConfigError::InvalidTerminalTransparency(101))
        ));
    }

    #[test]
    fn replacements_are_deterministic() {
        let theme = ThemeColors {
            text: Rgb(1, 2, 3),
            ..ThemeColors::default()
        };
        let mut bindings = BTreeMap::new();
        bindings.insert("copy".into(), "Cmd-C".into());
        let config = TerminalConfig::default()
            .with_theme(theme.clone())
            .with_keybindings(KeybindingConfig {
                profile: KeybindingProfile::Vim,
                bindings: bindings.clone(),
            });
        assert_eq!(config.theme, theme);
        assert_eq!(config.keybindings.bindings, bindings);
    }

    #[test]
    fn version_compatibility_is_explicit() {
        assert!(ConfigVersion::CURRENT.is_compatible());
        let config = TerminalConfig {
            version: ConfigVersion(99),
            ..TerminalConfig::default()
        };
        assert!(matches!(
            config.validate(),
            Err(ConfigError::UnsupportedVersion(ConfigVersion(99)))
        ));
    }
}
