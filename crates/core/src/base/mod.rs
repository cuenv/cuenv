//! Legacy `env.cue`-specific Base inventory.
//!
//! Module-wide operations use recursive selected-package evaluation instead;
//! this module remains only for compatibility with filename-specific callers.

pub mod discovery;

pub use discovery::{BaseDiscovery, BaseEvalFn, DiscoveredBase, DiscoveryError};
