//! Provider detection shared by CI execution and generated-file sync.
//!
//! - [`detect_ci_provider`] - Detect the appropriate CI provider
//! - [`detect_code_owners_provider`] - Detect the appropriate CODEOWNERS provider
//! - [`evaluate_rules_file`] - Evaluate a `.rules.cue` file in isolation

mod detection;
mod rules_eval;

pub use detection::{detect_ci_provider, detect_code_owners_provider};
pub use rules_eval::evaluate_rules_file;
