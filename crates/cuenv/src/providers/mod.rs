//! Provider detection and rules-file evaluation helpers.
//!
//! - [`detect_ci_provider`] - Detect the appropriate CI provider
//! - [`detect_code_owners_provider`] - Detect the appropriate CODEOWNERS provider
//! - [`evaluate_rules_file`] - Evaluate a `.rules.cue` file in isolation

mod detection;
pub(crate) mod rules_eval;

pub use detection::{detect_ci_provider, detect_code_owners_provider};
pub use rules_eval::evaluate_rules_file;
