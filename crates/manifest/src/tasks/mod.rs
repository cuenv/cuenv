//! Task DTO types matching the CUE schema.
//!
//! # Task API v2
//!
//! Users annotate tasks with their type to unlock specific semantics:
//! - [`Task`]: Single command or script
//! - [`TaskGroup`]: Parallel execution (all children run concurrently)
//! - Sequences (`Vec<TaskNode>`): Sequential execution (steps run in order)
//!
//! Execution behavior (command resolution, affected-detection, graph
//! building) lives in `cuenv-core`; this module holds the serde types and
//! pure accessors only.

mod cache_policy;
mod capture_types;
mod dagger;
mod dependency;
mod group;
mod hermetic;
mod inputs;
mod node;
mod params;
mod resolver;
mod retry;
mod shell;
mod task;

pub use cache_policy::{TaskCacheMode, TaskCachePolicy};
pub use capture_types::{CaptureSource, TaskCapture, TaskCaptureRef};
pub use dagger::{DaggerCacheMount, DaggerSecret, DaggerTaskConfig};
pub use dependency::TaskDependency;
pub use group::TaskGroup;
pub use hermetic::{Hermetic, HermeticOptions, Sandbox, SandboxPolicy};
pub use inputs::{
    Input, MappedInput, Mapping, ProjectReference, SourceLocation, TaskDirectory,
    TaskDirectoryBase, TaskOutput,
};
pub use node::{TaskNode, Tasks};
pub use params::{ParamDef, ParamType, ResolvedArgs, TaskParams};
pub use retry::RetryConfig;
pub use shell::{ScriptShell, Shell, ShellOptionToggle, ShellOptions};
pub use task::Task;
