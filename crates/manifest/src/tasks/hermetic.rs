//! Hermeticity configuration for a task.
//!
//! `hermetic` accepts either a bare bool — the historical spelling — or an
//! options struct that carries the knobs isolation needs. [`Hermetic`]
//! unifies the two so call sites ask [`Hermetic::is_enabled`] and
//! [`Hermetic::passthrough`] rather than matching on the shape.

use serde::{Deserialize, Serialize};

/// Options form of a task's `hermetic` field.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HermeticOptions {
    /// Host environment variable names the action is allowed to observe.
    ///
    /// Names listed here have their host values folded into the action's
    /// cache key; names absent from the list never enter the key, so a task
    /// whose result depends on an undeclared host variable is not portable
    /// and cuenv will not pretend otherwise.
    #[serde(default)]
    pub passthrough: Vec<String>,
}

/// A task's `hermetic` setting.
///
/// `hermetic: true` / `hermetic: false` deserialize to [`Hermetic::Enabled`];
/// `hermetic: {passthrough: [...]}` deserializes to [`Hermetic::Options`].
/// The options form always implies hermeticity is on — there would be
/// nothing to configure otherwise.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum Hermetic {
    /// Bare bool form.
    Enabled(bool),
    /// Options form.
    Options(HermeticOptions),
}

impl Default for Hermetic {
    fn default() -> Self {
        Self::Enabled(true)
    }
}

impl Hermetic {
    /// Whether the task opts into hermetic execution.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        match self {
            Self::Enabled(enabled) => *enabled,
            Self::Options(_) => true,
        }
    }

    /// Host environment variable names permitted into the action key.
    #[must_use]
    pub fn passthrough(&self) -> &[String] {
        match self {
            Self::Enabled(_) => &[],
            Self::Options(options) => &options.passthrough,
        }
    }
}

impl From<bool> for Hermetic {
    fn from(enabled: bool) -> Self {
        Self::Enabled(enabled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bool_form_round_trips() {
        let on: Hermetic = serde_json::from_str("true").unwrap();
        assert!(on.is_enabled());
        assert!(on.passthrough().is_empty());

        let off: Hermetic = serde_json::from_str("false").unwrap();
        assert!(!off.is_enabled());
    }

    #[test]
    fn options_form_parses_passthrough() {
        let parsed: Hermetic = serde_json::from_str(r#"{"passthrough":["HOME","CI"]}"#).unwrap();
        assert!(parsed.is_enabled());
        assert_eq!(parsed.passthrough(), ["HOME".to_string(), "CI".to_string()]);
    }

    #[test]
    fn empty_options_object_is_hermetic_with_no_passthrough() {
        let parsed: Hermetic = serde_json::from_str("{}").unwrap();
        assert!(parsed.is_enabled());
        assert!(parsed.passthrough().is_empty());
    }

    #[test]
    fn unknown_option_is_rejected() {
        // Without `deny_unknown_fields` an untagged enum swallows typos
        // silently, which would turn a misspelled knob into a no-op.
        let parsed: Result<Hermetic, _> = serde_json::from_str(r#"{"passthrouhg":["HOME"]}"#);
        assert!(parsed.is_err());
    }

    #[test]
    fn default_is_enabled() {
        assert!(Hermetic::default().is_enabled());
    }

    #[test]
    fn serializes_back_to_its_input_shape() {
        assert_eq!(serde_json::to_string(&Hermetic::Enabled(false)).unwrap(), "false");
        assert_eq!(
            serde_json::to_string(&Hermetic::Options(HermeticOptions {
                passthrough: vec!["HOME".into()],
            }))
            .unwrap(),
            r#"{"passthrough":["HOME"]}"#
        );
    }
}
