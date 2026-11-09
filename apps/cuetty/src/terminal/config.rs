//! Renderer-independent terminal configuration.
//!
//! This module deliberately has no persistence dependency. [`ConfigStore`] and
//! [`ConfigCodec`] are the seams for a later package to add serde/file
//! persistence without coupling the terminal contract to a format today.

use std::collections::BTreeMap;
use std::fmt;

pub const CURRENT_CONFIG_VERSION: u16 = 1;
pub const MAX_TERMINAL_PADDING: u16 = 128;

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
    /// Whether the font's contextual alternates (`calt`) apply, which is what Monaspace's
    /// texture healing is built on. Ligatures (`liga`, `clig`) are always off.
    pub contextual_alternates: bool,
    /// CSS weight for bold text, 100 to 900. A monospaced 700 fills in small counters
    /// at terminal sizes, so the default stops short of it; a family without this exact
    /// weight uses its nearest.
    pub bold_weight: u16,
}

/// The contrast ratio text is kept to against what it is drawn on, from 1 (no
/// adjustment) to 21 (only black on white would do).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MinimumContrast(f32);

impl MinimumContrast {
    pub const OFF: Self = Self(1.0);
    /// What a mark needs to be seen when it carries no text: WCAG's non-text contrast.
    pub const NON_TEXT: Self = Self(3.0);

    /// `None` for what is not a contrast ratio.
    pub fn new(ratio: f32) -> Option<Self> {
        (ratio.is_finite() && (1.0..=21.0).contains(&ratio)).then_some(Self(ratio))
    }

    pub const fn ratio(self) -> f32 {
        self.0
    }

    pub fn is_off(self) -> bool {
        self.0 <= Self::OFF.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalPadding {
    pub top: u16,
    pub right: u16,
    pub bottom: u16,
    pub left: u16,
}

impl Default for TerminalPadding {
    fn default() -> Self {
        Self {
            top: 14,
            right: 14,
            bottom: 14,
            left: 14,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorPreference {
    Block,
    Bar,
    Underline,
}

/// Whether the cursor blinks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CursorBlink {
    /// As the running application asks, which leaves it steady at most prompts.
    #[default]
    Terminal,
    On,
    Off,
}

/// How much the chrome may move.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MotionPreference {
    /// As the platform's accessibility setting says, where one can be read.
    #[default]
    System,
    Full,
    /// Nothing travels: transitions land at once and the cursor leaves no trail.
    Reduced,
}

/// A complete palette Cuetty ships, selected by the `theme` setting.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ThemeName {
    #[default]
    CatppuccinMocha,
    CatppuccinLatte,
    Graphite,
    Paper,
}

impl ThemeName {
    pub const ALL: [Self; 4] = [
        Self::CatppuccinMocha,
        Self::CatppuccinLatte,
        Self::Graphite,
        Self::Paper,
    ];

    /// What the `theme` setting calls this palette.
    pub const fn id(self) -> &'static str {
        match self {
            Self::CatppuccinMocha => "catppuccin-mocha",
            Self::CatppuccinLatte => "catppuccin-latte",
            Self::Graphite => "graphite",
            Self::Paper => "paper",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|theme| theme.id() == id)
    }

    pub fn colors(self) -> ThemeColors {
        match self {
            Self::CatppuccinMocha => ThemeColors::catppuccin_mocha(),
            Self::CatppuccinLatte => ThemeColors::catppuccin_latte(),
            Self::Graphite => ThemeColors::graphite(),
            Self::Paper => ThemeColors::paper(),
        }
    }
}

/// Whether the desktop a window sits on is light or dark.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SystemAppearance {
    Light,
    #[default]
    Dark,
}

/// What the `theme` setting asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeChoice {
    Named(ThemeName),
    /// `"auto"`: one flavour of Catppuccin for each appearance, switching as the
    /// desktop does.
    FollowSystem,
}

impl Default for ThemeChoice {
    fn default() -> Self {
        Self::Named(ThemeName::default())
    }
}

impl ThemeChoice {
    const FOLLOW_SYSTEM_ID: &str = "auto";

    pub fn from_id(id: &str) -> Option<Self> {
        if id == Self::FOLLOW_SYSTEM_ID {
            return Some(Self::FollowSystem);
        }
        ThemeName::from_id(id).map(Self::Named)
    }

    /// Everything the setting accepts, for saying so when it is given something else.
    pub fn ids() -> impl Iterator<Item = &'static str> {
        std::iter::once(Self::FOLLOW_SYSTEM_ID).chain(ThemeName::ALL.map(ThemeName::id))
    }

    const fn name_for(self, appearance: SystemAppearance) -> ThemeName {
        match (self, appearance) {
            (Self::Named(name), _) => name,
            (Self::FollowSystem, SystemAppearance::Light) => ThemeName::CatppuccinLatte,
            (Self::FollowSystem, SystemAppearance::Dark) => ThemeName::CatppuccinMocha,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeColors {
    pub surface: Rgb,
    pub title_surface: Rgb,
    pub title_text: Rgb,
    pub text: Rgb,
    pub cursor: Rgb,
    pub cursor_text: Rgb,
    /// Selection tint painted between cell backgrounds and glyphs.
    pub selection: Rgb,
    /// Every other search result on screen: a quiet tint beneath text that keeps its own
    /// colours, as a selection is.
    pub search_match: Rgb,
    /// The active search result; stays distinct where it overlaps a selection.
    pub current_search_match: Rgb,
    /// Glyphs inside the active search result, which sit on an opaque fill no
    /// cell foreground was chosen to read against.
    pub current_search_match_text: Rgb,
    /// The single restrained accent chrome may use for small markers.
    pub accent: Rgb,
    /// Hints and metadata, quieter than `title_text`.
    pub muted_text: Rgb,
    /// Faint line separating chrome regions and outlining floating panels.
    pub divider: Rgb,
    /// One step above the chrome: floating panels and hovered controls.
    pub raised_surface: Rgb,
    /// Hovered rows on a floating panel. Rows keep muted metadata when hovered,
    /// so this fill has to stay legible beneath it.
    pub hover_surface: Rgb,
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
    /// Percentage of the window's one material that remains transparent: the
    /// terminal canvas and structural chrome share it, and it selects an opaque
    /// or blurred compositor appearance. Zero is fully opaque and 100 is fully
    /// transparent.
    pub terminal_transparency_percent: u8,
    pub terminal_padding: TerminalPadding,
    pub minimum_contrast: MinimumContrast,
    pub cursor: CursorPreference,
    pub cursor_blink: CursorBlink,
    /// A short fading trail behind a cursor that jumps. Off unless asked for.
    pub cursor_trail: bool,
    pub motion: MotionPreference,
    /// What the theme setting asked for, which `theme` is resolved from.
    pub theme_choice: ThemeChoice,
    /// The `accent` setting, kept to reapply when `auto` changes the palette under it.
    pub accent_override: Option<Rgb>,
    /// The palette in force: see [`TerminalConfig::for_appearance`].
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
    InvalidBoldWeight(u16),
    InvalidTerminalTransparency(u8),
    InvalidTerminalPadding(u16),
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
            Self::InvalidBoldWeight(value) => {
                write!(f, "bold weight must be between 100 and 900, got {value}")
            }
            Self::InvalidTerminalTransparency(value) => write!(
                f,
                "terminal transparency must be between 0 and 100%, got {value}%"
            ),
            Self::InvalidTerminalPadding(value) => {
                write!(
                    f,
                    "terminal padding must be between 0 and 128px, got {value}px"
                )
            }
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
                contextual_alternates: true,
                bold_weight: 600,
            },
            terminal_transparency_percent: 0,
            terminal_padding: TerminalPadding::default(),
            minimum_contrast: MinimumContrast::OFF,
            cursor: CursorPreference::Block,
            cursor_blink: CursorBlink::default(),
            cursor_trail: false,
            motion: MotionPreference::default(),
            theme_choice: ThemeChoice::default(),
            accent_override: None,
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
            // Base fills the window; Mantle sets the sidebar off from it.
            surface: Rgb(30, 30, 46),
            title_surface: Rgb(24, 24, 37),
            title_text: Rgb(186, 194, 222),
            text: Rgb(205, 214, 244),
            cursor: Rgb(245, 224, 230),
            cursor_text: Rgb(30, 30, 46),
            // Short of Surface 1, the tint Catppuccin publishes: on that, or on anything
            // brighter, the ANSI red and blue an application prints fall under 4.5:1.
            selection: Rgb(60, 61, 79),
            // A dark Yellow, the current match's hue, so the two read as one search. It
            // stops at the same brightness as the selection, for the same reason.
            search_match: Rgb(68, 60, 50),
            current_search_match: Rgb(249, 226, 175),
            current_search_match_text: Rgb(17, 17, 27),
            accent: Rgb(137, 180, 250),
            muted_text: Rgb(147, 153, 178),
            divider: Rgb(49, 50, 68),
            // Between Base and Surface 0: Surface 0 itself is too bright to
            // fill a whole panel and would swallow the divider around it.
            raised_surface: Rgb(37, 37, 55),
            hover_surface: Rgb(45, 45, 65),
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

    /// Catppuccin Latte, for a light desktop.
    ///
    /// Latte's published colours are its neutrals and accents, but several of them were
    /// not chosen to carry text at terminal sizes, where this has to hold 4.5:1: the
    /// metadata tones are a step darker than Subtext, the cursor is Text rather than the
    /// 2.4:1 Rosewater, and every ANSI hue, red and blue included, is deepened until it
    /// reads on the selection as well as on the page. Hues stay Latte's. ANSI white stays
    /// faint, as black does on a dark theme.
    pub fn catppuccin_latte() -> Self {
        Self {
            surface: Rgb(239, 241, 245),
            title_surface: Rgb(230, 233, 239),
            title_text: Rgb(84, 87, 110),
            text: Rgb(76, 79, 105),
            cursor: Rgb(76, 79, 105),
            cursor_text: Rgb(239, 241, 245),
            selection: Rgb(217, 219, 227),
            search_match: Rgb(241, 227, 196),
            current_search_match: Rgb(223, 142, 29),
            current_search_match_text: Rgb(17, 17, 27),
            accent: Rgb(30, 102, 245),
            muted_text: Rgb(95, 98, 122),
            divider: Rgb(212, 216, 225),
            // Above Base on a light theme means towards white, which is also how a
            // panel lifts off the page beneath its shadow.
            raised_surface: Rgb(247, 248, 250),
            hover_surface: Rgb(228, 231, 237),
            ansi: [
                Rgb(76, 79, 105),
                Rgb(190, 13, 51),
                Rgb(40, 109, 25),
                Rgb(131, 86, 10),
                Rgb(24, 86, 210),
                Rgb(159, 50, 136),
                Rgb(14, 105, 111),
                Rgb(172, 176, 190),
                Rgb(91, 94, 115),
                Rgb(190, 13, 51),
                Rgb(40, 109, 25),
                Rgb(131, 86, 10),
                Rgb(24, 86, 210),
                Rgb(159, 50, 136),
                Rgb(14, 105, 111),
                Rgb(188, 192, 204),
            ],
        }
    }

    /// A neutral dark theme: greys with no hue to them, one quiet steel accent.
    pub fn graphite() -> Self {
        Self {
            surface: Rgb(27, 28, 31),
            title_surface: Rgb(21, 22, 25),
            title_text: Rgb(184, 187, 193),
            text: Rgb(217, 219, 223),
            cursor: Rgb(228, 229, 232),
            cursor_text: Rgb(27, 28, 31),
            selection: Rgb(56, 58, 65),
            search_match: Rgb(61, 57, 47),
            current_search_match: Rgb(227, 194, 122),
            current_search_match_text: Rgb(20, 21, 24),
            accent: Rgb(143, 176, 214),
            muted_text: Rgb(146, 149, 156),
            divider: Rgb(45, 47, 51),
            raised_surface: Rgb(34, 35, 39),
            hover_surface: Rgb(43, 45, 49),
            ansi: [
                Rgb(67, 69, 75),
                Rgb(233, 139, 140),
                Rgb(150, 195, 125),
                Rgb(217, 184, 106),
                Rgb(129, 168, 217),
                Rgb(195, 148, 214),
                Rgb(111, 191, 191),
                Rgb(198, 200, 204),
                Rgb(90, 93, 100),
                Rgb(239, 143, 144),
                Rgb(170, 211, 146),
                Rgb(230, 200, 131),
                Rgb(150, 184, 227),
                Rgb(210, 169, 226),
                Rgb(134, 207, 207),
                Rgb(236, 238, 240),
            ],
        }
    }

    /// A warm light theme: ink on uncoated paper, one rust accent.
    pub fn paper() -> Self {
        Self {
            surface: Rgb(247, 243, 234),
            title_surface: Rgb(239, 233, 220),
            title_text: Rgb(69, 63, 54),
            text: Rgb(44, 40, 34),
            cursor: Rgb(44, 40, 34),
            cursor_text: Rgb(247, 243, 234),
            selection: Rgb(228, 221, 206),
            search_match: Rgb(240, 226, 180),
            current_search_match: Rgb(201, 138, 27),
            current_search_match_text: Rgb(23, 20, 15),
            accent: Rgb(156, 90, 44),
            muted_text: Rgb(100, 93, 81),
            divider: Rgb(224, 217, 202),
            raised_surface: Rgb(252, 250, 244),
            hover_surface: Rgb(237, 231, 218),
            ansi: [
                Rgb(44, 40, 34),
                Rgb(164, 51, 31),
                Rgb(67, 100, 28),
                Rgb(116, 87, 0),
                Rgb(47, 92, 151),
                Rgb(134, 64, 127),
                Rgb(26, 101, 101),
                Rgb(203, 195, 177),
                Rgb(97, 90, 79),
                Rgb(172, 57, 36),
                Rgb(71, 106, 31),
                Rgb(123, 92, 0),
                Rgb(54, 97, 156),
                Rgb(143, 69, 135),
                Rgb(28, 107, 107),
                Rgb(215, 207, 191),
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
        if !(100..=900).contains(&self.font.bold_weight) {
            return Err(ConfigError::InvalidBoldWeight(self.font.bold_weight));
        }
        if self.terminal_transparency_percent > 100 {
            return Err(ConfigError::InvalidTerminalTransparency(
                self.terminal_transparency_percent,
            ));
        }
        if [
            self.terminal_padding.top,
            self.terminal_padding.right,
            self.terminal_padding.bottom,
            self.terminal_padding.left,
        ]
        .into_iter()
        .any(|value| value > MAX_TERMINAL_PADDING)
        {
            let value = [
                self.terminal_padding.top,
                self.terminal_padding.right,
                self.terminal_padding.bottom,
                self.terminal_padding.left,
            ]
            .into_iter()
            .max()
            .expect("terminal padding always has four edges");
            return Err(ConfigError::InvalidTerminalPadding(value));
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

    /// Applies the `theme` and `accent` settings. A theme that follows the system starts
    /// out dark until [`Self::for_appearance`] is told otherwise.
    pub fn choose_theme(&mut self, choice: ThemeChoice, accent: Option<Rgb>) {
        self.theme_choice = choice;
        self.accent_override = accent;
        self.theme = choice.name_for(SystemAppearance::default()).colors();
        if let Some(accent) = accent {
            self.theme.accent = accent;
        }
    }

    /// This configuration as a window of the given appearance paints it. Only `auto`
    /// depends on that; any other theme is left exactly as it stands.
    #[must_use]
    pub fn for_appearance(mut self, appearance: SystemAppearance) -> Self {
        self.theme = self.colors_for(appearance);
        self
    }

    /// The palette a window of the given appearance paints in, without the rest.
    pub fn colors_for(&self, appearance: SystemAppearance) -> ThemeColors {
        match self.theme_choice {
            ThemeChoice::Named(_) => self.theme.clone(),
            ThemeChoice::FollowSystem => {
                let mut colors = self.theme_choice.name_for(appearance).colors();
                if let Some(accent) = self.accent_override {
                    colors.accent = accent;
                }
                colors
            }
        }
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
        assert_eq!(config.terminal_padding, TerminalPadding::default());
        assert_eq!(config.theme.surface, Rgb(30, 30, 46));
        assert_eq!(config.theme.title_surface, Rgb(24, 24, 37));
        assert_eq!(config.theme.text, Rgb(205, 214, 244));
        assert_eq!(config.theme.ansi[1], Rgb(243, 139, 168));
        assert_eq!(config.theme.ansi[4], Rgb(137, 180, 250));
        assert_eq!(config.theme.ansi[5], Rgb(245, 194, 231));
        assert!(config.validate().is_ok());
    }

    /// WCAG 2.1 relative luminance.
    fn luminance(Rgb(red, green, blue): Rgb) -> f64 {
        let channel = |value: u8| {
            let value = f64::from(value) / 255.0;
            if value <= 0.03928 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(red) + 0.7152 * channel(green) + 0.0722 * channel(blue)
    }

    fn contrast(first: Rgb, second: Rgb) -> f64 {
        let (first, second) = (luminance(first), luminance(second));
        (first.max(second) + 0.05) / (first.min(second) + 0.05)
    }

    /// Whether a theme's page is light, which decides which ANSI neutral is the faint one.
    fn is_light(theme: &ThemeColors) -> bool {
        luminance(theme.surface) > 0.5
    }

    fn for_every_theme(check: impl Fn(ThemeName, &ThemeColors)) {
        for name in ThemeName::ALL {
            check(name, &name.colors());
        }
    }

    struct TextSample {
        label: &'static str,
        ink: Rgb,
    }

    fn assert_legible(name: ThemeName, text: TextSample, fills: &[Rgb]) {
        for fill in fills {
            let ratio = contrast(text.ink, *fill);
            assert!(
                ratio >= 4.5,
                "{}: {} {:?} on {fill:?} is only {ratio:.2}:1",
                name.id(),
                text.label,
                text.ink
            );
        }
    }

    #[test]
    fn body_text_stays_legible_on_every_fill_it_is_painted_on() {
        for_every_theme(|name, theme| {
            assert_legible(
                name,
                TextSample {
                    label: "text",
                    ink: theme.text,
                },
                &[
                    theme.surface,
                    theme.title_surface,
                    theme.raised_surface,
                    theme.hover_surface,
                    theme.selection,
                    theme.search_match,
                ],
            );
        });
    }

    #[test]
    fn secondary_text_stays_legible_on_selected_rows_as_well() {
        for_every_theme(|name, theme| {
            assert_legible(
                name,
                TextSample {
                    label: "title text",
                    ink: theme.title_text,
                },
                &[
                    theme.surface,
                    theme.title_surface,
                    theme.raised_surface,
                    theme.hover_surface,
                    theme.selection,
                ],
            );
        });
    }

    #[test]
    fn muted_text_stays_legible_on_every_fill_it_is_painted_on() {
        for_every_theme(|name, theme| {
            assert_legible(
                name,
                TextSample {
                    label: "muted text",
                    ink: theme.muted_text,
                },
                &[
                    theme.title_surface,
                    theme.surface,
                    theme.raised_surface,
                    theme.hover_surface,
                ],
            );
        });
    }

    #[test]
    fn text_on_a_solid_fill_of_its_own_stays_legible() {
        for_every_theme(|name, theme| {
            assert_legible(
                name,
                TextSample {
                    label: "cursor text",
                    ink: theme.cursor_text,
                },
                &[theme.cursor],
            );
            assert_legible(
                name,
                TextSample {
                    label: "current match text",
                    ink: theme.current_search_match_text,
                },
                &[theme.current_search_match],
            );
        });
    }

    /// Black on a dark theme and white on a light one are the colours applications use for
    /// what they mean to be barely there, so those two only have to stay visible. Every
    /// other colour is something an application prints for reading, and it keeps its colour
    /// when selected or found, so it has to read on those tints as well as on the page.
    #[test]
    fn ansi_colours_are_legible_wherever_they_are_painted_apart_from_the_faint_neutrals() {
        for_every_theme(|name, theme| {
            let faint = if is_light(theme) { [7, 15] } else { [0, 8] };
            let tints = [theme.selection, theme.search_match];
            for (index, color) in theme.ansi.iter().enumerate() {
                if faint.contains(&index) {
                    let on_page = contrast(*color, theme.surface);
                    assert!(on_page >= 1.3, "{}: {index} {on_page:.2}:1", name.id());
                    for tint in tints {
                        let on_tint = contrast(*color, tint);
                        assert!(on_tint >= 1.1, "{}: {index} {on_tint:.2}:1", name.id());
                    }
                } else {
                    assert_legible(
                        name,
                        TextSample {
                            label: "ANSI colour",
                            ink: *color,
                        },
                        &[theme.surface],
                    );
                    assert_legible(
                        name,
                        TextSample {
                            label: "ANSI colour",
                            ink: *color,
                        },
                        &tints,
                    );
                }
            }
        });
    }

    #[test]
    fn the_failed_tab_mark_reads_on_the_sidebar_and_the_panels() {
        for_every_theme(|name, theme| {
            assert_legible(
                name,
                TextSample {
                    label: "danger",
                    ink: theme.ansi[1],
                },
                &[theme.title_surface, theme.raised_surface],
            );
        });
    }

    #[test]
    fn marks_that_carry_no_text_still_stand_out() {
        for_every_theme(|name, theme| {
            for fill in [
                theme.surface,
                theme.title_surface,
                theme.raised_surface,
                theme.hover_surface,
            ] {
                let accent = contrast(theme.accent, fill);
                assert!(accent >= 3.0, "{}: accent {accent:.2}:1", name.id());
            }
            let cursor = contrast(theme.cursor, theme.surface);
            assert!(cursor >= 3.0, "{}: cursor {cursor:.2}:1", name.id());
        });
    }

    #[test]
    fn dividers_stay_faint_but_visible_and_surfaces_stay_apart() {
        for_every_theme(|name, theme| {
            for surface in [theme.title_surface, theme.surface, theme.raised_surface] {
                let divider = contrast(theme.divider, surface);
                assert!(
                    (1.1..2.0).contains(&divider),
                    "{}: divider contrast {divider:.2}",
                    name.id()
                );
            }
            let steps = [
                (
                    "raised on sidebar",
                    theme.raised_surface,
                    theme.title_surface,
                    1.03,
                ),
                ("raised on page", theme.raised_surface, theme.surface, 1.03),
                ("sidebar on page", theme.title_surface, theme.surface, 1.03),
                (
                    "row hover on panel",
                    theme.hover_surface,
                    theme.raised_surface,
                    1.1,
                ),
                (
                    "selected row beside a hovered one",
                    theme.selection,
                    theme.hover_surface,
                    1.05,
                ),
                ("selection on page", theme.selection, theme.surface, 1.2),
                (
                    "search match on page",
                    theme.search_match,
                    theme.surface,
                    1.1,
                ),
                (
                    "current match on page",
                    theme.current_search_match,
                    theme.surface,
                    1.5,
                ),
            ];
            for (what, fill, ground, least) in steps {
                let step = contrast(fill, ground);
                assert!(step >= least, "{}: {what} {step:.2}:1", name.id());
            }
        });
    }

    #[test]
    fn every_theme_goes_by_the_name_the_setting_uses_for_it() {
        for name in ThemeName::ALL {
            assert_eq!(ThemeName::from_id(name.id()), Some(name));
        }
        assert_eq!(ThemeName::from_id("Catppuccin Mocha"), None);
        assert_eq!(ThemeColors::default(), ThemeName::default().colors());
    }

    #[test]
    fn auto_switches_palette_with_the_appearance_on_either_side_of_light() {
        let mut config = TerminalConfig::default();
        config.choose_theme(ThemeChoice::FollowSystem, None);
        let on = |appearance| config.clone().for_appearance(appearance).theme;
        let (light, dark) = (on(SystemAppearance::Light), on(SystemAppearance::Dark));
        assert!(is_light(&light));
        assert!(!is_light(&dark));
    }

    #[test]
    fn auto_keeps_a_configured_accent_through_a_switch() {
        let accent = Rgb(1, 2, 3);
        let mut config = TerminalConfig::default();
        config.choose_theme(ThemeChoice::FollowSystem, Some(accent));
        let config = config
            .for_appearance(SystemAppearance::Light)
            .for_appearance(SystemAppearance::Dark);
        assert_eq!(config.theme.accent, accent);
        assert_eq!(
            config.theme.surface,
            ThemeName::CatppuccinMocha.colors().surface
        );
    }

    #[test]
    fn a_named_theme_ignores_the_appearance() {
        let mut config = TerminalConfig::default();
        config.choose_theme(ThemeChoice::Named(ThemeName::Paper), None);
        for appearance in [SystemAppearance::Light, SystemAppearance::Dark] {
            assert_eq!(
                config.clone().for_appearance(appearance).theme,
                ThemeName::Paper.colors()
            );
        }
    }

    #[test]
    fn the_setting_accepts_auto_and_every_theme() {
        assert_eq!(
            ThemeChoice::from_id("auto"),
            Some(ThemeChoice::FollowSystem)
        );
        for name in ThemeName::ALL {
            assert_eq!(
                ThemeChoice::from_id(name.id()),
                Some(ThemeChoice::Named(name))
            );
        }
        assert_eq!(ThemeChoice::from_id("Auto"), None);
        assert!(ThemeChoice::ids().all(|id| ThemeChoice::from_id(id).is_some()));
    }

    #[test]
    fn themes_are_told_apart_and_two_of_them_are_light() {
        let surfaces: Vec<Rgb> = ThemeName::ALL
            .iter()
            .map(|name| name.colors().surface)
            .collect();
        for (index, surface) in surfaces.iter().enumerate() {
            assert!(!surfaces[..index].contains(surface));
        }
        assert_eq!(
            ThemeName::ALL
                .into_iter()
                .filter(|name| is_light(&name.colors()))
                .count(),
            2
        );
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
        let config = TerminalConfig {
            terminal_padding: TerminalPadding {
                right: 129,
                ..TerminalPadding::default()
            },
            ..TerminalConfig::default()
        };
        assert_eq!(
            config.validate(),
            Err(ConfigError::InvalidTerminalPadding(129))
        );
        for weight in [99, 901] {
            let mut config = TerminalConfig::default();
            config.font.bold_weight = weight;
            assert_eq!(
                config.validate(),
                Err(ConfigError::InvalidBoldWeight(weight))
            );
        }
    }

    #[test]
    fn a_minimum_contrast_has_to_be_a_contrast_ratio() {
        for invalid in [0.99, 21.01, f32::NAN, f32::INFINITY] {
            assert_eq!(MinimumContrast::new(invalid), None);
        }
        assert!(MinimumContrast::new(1.0).is_some_and(MinimumContrast::is_off));
        assert!(MinimumContrast::new(4.5).is_some_and(|contrast| !contrast.is_off()));
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
