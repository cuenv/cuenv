//! Root Project configuration type
//!
//! Based on schema/core.cue

mod codegen;
mod formatters;
mod hooks;
mod project;
mod rules;
mod runtime;
mod services;
mod vcs;

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::config::Config;
use crate::environment::Env;
use cuenv_hooks::Hooks;

pub use codegen::*;
pub use formatters::*;
pub use hooks::*;
pub use project::Project;
pub use rules::*;
pub use runtime::*;
pub use services::*;
pub use vcs::VcsDependency;

/// Base configuration structure (composable across directories)
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Base {
    /// Configuration settings
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config: Option<Config>,

    /// Environment variables configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<Env>,

    /// Formatters configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub formatters: Option<Formatters>,

    /// Runtime configuration (devenv, nix, tools, etc.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<Runtime>,

    /// Hooks configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hooks: Option<Hooks>,

    /// Cuenv-managed VCS dependencies.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub vcs: HashMap<String, VcsDependency>,
}

#[cfg(test)]
#[path = "manifest_tests.rs"]
mod tests;
