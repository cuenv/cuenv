//! Hermeticity configuration for a task.
//!
//! `hermetic` accepts either a bare bool — the historical spelling — or an
//! options struct that carries the knobs isolation needs. [`Hermetic`]
//! unifies the two so call sites ask [`Hermetic::is_enabled`] and
//! [`Hermetic::passthrough`] rather than matching on the shape.

use serde::{Deserialize, Serialize};

/// How much filesystem isolation a task runs under.
///
/// [`Sandbox::Dir`] runs the task in a per-action directory populated from
/// declared inputs, isolating relative workspace reads and writes.
/// It is not an OS security boundary: absolute host paths remain reachable.
/// [`Sandbox::None`] runs directly in the project directory.
///
/// [`Sandbox::Dir`] is the default so relative workspace access follows the
/// declarations on the first run as well as on cache hits.
///
/// Stricter OS-level tiers are deliberately absent until they exist.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Sandbox {
    /// Directory isolation for relative workspace access, with only declared
    /// outputs projected back. The default.
    #[default]
    Dir,
    /// No isolation: the project directory, unrestricted. The explicit
    /// opt-out, for tasks that must touch the live checkout.
    None,
}

impl Sandbox {
    /// Whether this tier runs the task in a per-action exec root.
    #[must_use]
    pub fn uses_exec_root(self) -> bool {
        match self {
            Self::None => false,
            Self::Dir => true,
        }
    }
}

/// Options form of a task's `hermetic` field.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HermeticOptions {
    /// Host environment variable names the action is allowed to observe.
    ///
    /// Names listed here have their host values folded into the action's
    /// cache key; names absent from the list never enter the key, so a task
    /// whose result depends on an undeclared host variable is not portable
    /// and cuenv will not pretend otherwise. `HOME` is forbidden because
    /// tasks that need the caller's home must opt out of hermetic execution.
    #[serde(default)]
    pub passthrough: Vec<String>,

    /// Filesystem isolation tier. Absent means the default, [`Sandbox::Dir`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<Sandbox>,
}

/// A task's `hermetic` setting.
///
/// `hermetic: true` / `hermetic: false` deserialize to [`Hermetic::Enabled`];
/// `hermetic: {passthrough: [...]}` deserializes to [`Hermetic::Options`].
/// The options form always implies hermeticity is on — there would be
/// nothing to configure otherwise.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum Hermetic {
    /// Bare bool form.
    Enabled(bool),
    /// Options form.
    Options(HermeticOptions),
}

impl<'de> Deserialize<'de> for Hermetic {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum HermeticRepr {
            Enabled(bool),
            Options(HermeticOptions),
        }

        match HermeticRepr::deserialize(deserializer)? {
            HermeticRepr::Enabled(enabled) => Ok(Self::Enabled(enabled)),
            HermeticRepr::Options(options) => {
                if options
                    .passthrough
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case("HOME"))
                {
                    return Err(serde::de::Error::custom(
                        "hermetic tasks cannot pass through host HOME; set hermetic: false to inherit the caller's environment",
                    ));
                }
                Ok(Self::Options(options))
            }
        }
    }
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

    /// Filesystem isolation tier for this task, and whether the user named it.
    ///
    /// A task that opted out of hermeticity entirely gets no isolation: there
    /// would be nothing coherent to isolate, since its key records neither
    /// its inputs nor its environment. Everything else defaults to
    /// [`Sandbox::Dir`].
    #[must_use]
    pub fn sandbox(&self) -> Sandbox {
        match self {
            Self::Enabled(false) => Sandbox::None,
            Self::Enabled(true) => Sandbox::Dir,
            Self::Options(options) => options.sandbox.unwrap_or(Sandbox::Dir),
        }
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
        let parsed: Hermetic =
            serde_json::from_str(r#"{"passthrough":["CARGO_HOME","CI"]}"#).unwrap();
        assert!(parsed.is_enabled());
        assert_eq!(
            parsed.passthrough(),
            ["CARGO_HOME".to_string(), "CI".to_string()]
        );
    }

    #[test]
    fn empty_options_object_is_hermetic_with_no_passthrough() {
        let parsed: Hermetic = serde_json::from_str("{}").unwrap();
        assert!(parsed.is_enabled());
        assert!(parsed.passthrough().is_empty());
    }

    #[test]
    fn a_hermetic_task_is_sandboxed_by_default() {
        // Bazel sandboxes local actions by default. A declaration that is
        // only enforced when asked is not a declaration.
        let bare: Hermetic = serde_json::from_str("true").unwrap();
        assert_eq!(bare.sandbox(), Sandbox::Dir);
        let options: Hermetic = serde_json::from_str(r#"{"passthrough":["CI"]}"#).unwrap();
        assert_eq!(options.sandbox(), Sandbox::Dir);
    }

    #[test]
    fn isolation_can_be_opted_out_of_explicitly() {
        let parsed: Hermetic = serde_json::from_str(r#"{"sandbox":"none"}"#).unwrap();
        assert_eq!(parsed.sandbox(), Sandbox::None);
        assert!(!parsed.sandbox().uses_exec_root());
    }

    #[test]
    fn a_non_hermetic_task_gets_no_isolation() {
        // Nothing coherent to isolate: its key records neither its inputs nor
        // its environment.
        let off: Hermetic = serde_json::from_str("false").unwrap();
        assert_eq!(off.sandbox(), Sandbox::None);
        assert!(!off.sandbox().uses_exec_root());
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
        assert_eq!(
            serde_json::to_string(&Hermetic::Enabled(false)).unwrap(),
            "false"
        );
        assert_eq!(
            serde_json::to_string(&Hermetic::Options(HermeticOptions {
                passthrough: vec!["CARGO_HOME".into()],
                sandbox: None,
            }))
            .unwrap(),
            r#"{"passthrough":["CARGO_HOME"]}"#
        );
        assert_eq!(
            serde_json::to_string(&Hermetic::Options(HermeticOptions {
                passthrough: Vec::new(),
                sandbox: Some(Sandbox::Dir),
            }))
            .unwrap(),
            r#"{"passthrough":[],"sandbox":"dir"}"#
        );
    }
}
