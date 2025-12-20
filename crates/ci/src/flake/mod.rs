//! Nix flake.lock parsing and purity analysis
//!
//! This module provides functionality to:
//! - Parse flake.lock files (version 7 format)
//! - Detect unlocked/impure flake inputs
//! - Compute deterministic digests from locked inputs
//! - Enforce purity modes (strict, warning, override)

mod analyzer;
mod error;
mod lock;

pub use analyzer::{FlakeLockAnalyzer, PurityAnalysis, UnlockReason, UnlockedInput};
pub use error::FlakeLockError;
pub use lock::{FlakeLock, FlakeNode, InputRef, LockedInfo, OriginalInfo};
