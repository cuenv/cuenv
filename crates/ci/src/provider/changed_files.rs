//! What a provider found when it compared a run with its base.

use std::fmt;
use std::path::PathBuf;

/// The files a run changed, when the run has something to compare with.
///
/// Affected detection needs a base. A provider whose run has none says so
/// here: a guessed base, such as the parent of `HEAD`, reports that nothing
/// is affected for a run that was never compared with what came before it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangedFiles {
    /// The run has a base, and these files differ from it.
    Compared(Vec<PathBuf>),
    /// The run has no base it can be compared with, so no task can be shown
    /// to be unaffected.
    NoBase(NoBase),
}

impl ChangedFiles {
    /// The files known to differ from the base: none when there is no base.
    #[must_use]
    pub fn files(&self) -> &[PathBuf] {
        match self {
            Self::Compared(files) => files,
            Self::NoBase(_) => &[],
        }
    }

    /// Why the run has no base, when it has none.
    #[must_use]
    pub fn no_base(&self) -> Option<&NoBase> {
        match self {
            Self::Compared(_) => None,
            Self::NoBase(why) => Some(why),
        }
    }
}

/// Why a run has nothing to compare itself with.
///
/// Displayed, it is the line a run prints before it takes its whole
/// pipeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoBase {
    reason: String,
    remedy: Option<String>,
}

impl NoBase {
    /// A run with no base. `reason` names the kind of run in the provider's
    /// own terms, such as "Buildkite branch build".
    #[must_use]
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
            remedy: None,
        }
    }

    /// Say what gives such a run a base. Only for a remedy that works: a
    /// flag the run would ignore is not one.
    #[must_use]
    pub fn with_remedy(mut self, remedy: impl Into<String>) -> Self {
        self.remedy = Some(remedy.into());
        self
    }

    /// The kind of run this is.
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

impl fmt::Display for NoBase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "No comparable base ({}): running the whole pipeline",
            self.reason
        )?;
        match &self.remedy {
            Some(remedy) => write!(formatter, ". {remedy}"),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_without_a_base_says_so_and_why() {
        let why = NoBase::new("Buildkite branch build");

        assert_eq!(why.reason(), "Buildkite branch build");
        assert_eq!(
            why.to_string(),
            "No comparable base (Buildkite branch build): running the whole pipeline"
        );
    }

    #[test]
    fn a_remedy_follows_the_reason() {
        let why = NoBase::new("unrecognised CI system").with_remedy("Pass --from <ref>");

        assert_eq!(
            why.to_string(),
            "No comparable base (unrecognised CI system): running the whole pipeline. \
             Pass --from <ref>"
        );
    }

    #[test]
    fn a_run_without_a_base_knows_of_no_changed_file() {
        let compared = ChangedFiles::Compared(vec![PathBuf::from("a.txt")]);
        let no_base = ChangedFiles::NoBase(NoBase::new("tag push"));

        assert_eq!(compared.files(), [PathBuf::from("a.txt")]);
        assert_eq!(compared.no_base(), None);
        assert!(no_base.files().is_empty());
        assert_eq!(no_base.no_base().map(NoBase::reason), Some("tag push"));
    }
}
