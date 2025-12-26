//! Rules discovery for .rules.cue files.
//!
//! This module provides functionality to discover `.rules.cue` files
//! throughout a repository and evaluate them independently (no CUE unification).

mod discovery;

pub use discovery::{DiscoveredRules, RulesDiscovery, RulesDiscoveryError};
