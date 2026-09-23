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

/// A resolved isolation tier, and whether the user named it.
///
/// The distinction is the whole reason this is not a bare [`Sandbox`]. A task
/// that *asks* for [`Sandbox::Dir`] and cannot have it must fail: handing back
/// a result that looks sandboxed and is not would be worse than refusing. A
/// task that merely *inherited* the default and cannot have it runs
/// unsandboxed, because it never asked for a guarantee. Bazel draws the same
/// line: an action outside the sandboxed strategy is not an error, but a
/// `--strategy` you named and cannot have is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SandboxPolicy {
    /// The isolation tier itself.
    pub tier: Sandbox,
    /// Whether the tier came from the task, rather than from the default.
    pub explicit: bool,
}

impl SandboxPolicy {
    /// A tier the task named.
    #[must_use]
    pub fn requested(tier: Sandbox) -> Self {
        Self {
            tier,
            explicit: true,
        }
    }

    /// A tier inherited from the default.
    #[must_use]
    pub fn defaulted(tier: Sandbox) -> Self {
        Self {
            tier,
            explicit: false,
        }
    }

    /// Whether this policy runs the task in a per-action exec root.
    #[must_use]
    pub fn uses_exec_root(self) -> bool {
        self.tier.uses_exec_root()
    }

    /// Whether failing to deliver this policy is an error rather than a
    /// silent downgrade.
    #[must_use]
    pub fn demands_exec_root(self) -> bool {
        self.explicit && self.uses_exec_root()
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
    /// and cuenv will not pretend otherwise.
    #[serde(default)]
    pub passthrough: Vec<String>,

    /// Filesystem isolation tier. Absent means the default, [`Sandbox::Dir`],
    /// applied as a default rather than as a demand — see
    /// [`SandboxPolicy::explicit`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<Sandbox>,
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

    /// Filesystem isolation tier for this task, and whether the user named it.
    ///
    /// A task that opted out of hermeticity entirely gets no isolation: there
    /// would be nothing coherent to isolate, since its key records neither
    /// its inputs nor its environment. Everything else defaults to
    /// [`Sandbox::Dir`].
    #[must_use]
    pub fn sandbox(&self) -> SandboxPolicy {
        match self {
            Self::Enabled(false) => SandboxPolicy::defaulted(Sandbox::None),
            Self::Enabled(true) => SandboxPolicy::defaulted(Sandbox::Dir),
            Self::Options(options) => match options.sandbox {
                Some(tier) => SandboxPolicy::requested(tier),
                None => SandboxPolicy::defaulted(Sandbox::Dir),
            },
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
    fn a_hermetic_task_is_sandboxed_by_default() {
        // Bazel sandboxes local actions by default. A declaration that is
        // only enforced when asked is not a declaration.
        let bare: Hermetic = serde_json::from_str("true").unwrap();
        assert_eq!(bare.sandbox().tier, Sandbox::Dir);
        let options: Hermetic = serde_json::from_str(r#"{"passthrough":["HOME"]}"#).unwrap();
        assert_eq!(options.sandbox().tier, Sandbox::Dir);
    }

    #[test]
    fn the_default_tier_is_not_a_demand() {
        // A task that inherited the default and cannot be sandboxed runs
        // anyway; only a task that named the tier gets to fail.
        let bare: Hermetic = serde_json::from_str("true").unwrap();
        assert!(bare.sandbox().uses_exec_root());
        assert!(!bare.sandbox().demands_exec_root());

        let named: Hermetic = serde_json::from_str(r#"{"sandbox":"dir"}"#).unwrap();
        assert!(named.sandbox().demands_exec_root());
    }

    #[test]
    fn isolation_can_be_opted_out_of_explicitly() {
        let parsed: Hermetic = serde_json::from_str(r#"{"sandbox":"none"}"#).unwrap();
        assert_eq!(parsed.sandbox().tier, Sandbox::None);
        assert!(!parsed.sandbox().uses_exec_root());
    }

    #[test]
    fn a_non_hermetic_task_gets_no_isolation() {
        // Nothing coherent to isolate: its key records neither its inputs nor
        // its environment.
        let off: Hermetic = serde_json::from_str("false").unwrap();
        assert_eq!(off.sandbox().tier, Sandbox::None);
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
                passthrough: vec!["HOME".into()],
                sandbox: None,
            }))
            .unwrap(),
            r#"{"passthrough":["HOME"]}"#
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
