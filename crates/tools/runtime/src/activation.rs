//! Tool activation planning and environment mutation helpers.
//!
//! Tool activation is inferred from lockfile tool metadata so every execution
//! path (`tools activate`, `exec`, `task`, CI) applies the same environment
//! mutations.

mod activation_plan;
mod path_index;

pub use activation_plan::{
    ResolvedToolActivationStep, ToolActivationResolveOptions, apply_resolved_tool_activation,
    resolve_tool_activation, validate_tool_activation,
};
pub use cuenv_manifest::tools::{
    ToolActivationOperation, ToolActivationSource, ToolActivationStep,
};
