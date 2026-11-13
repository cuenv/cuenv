//! Go-Rust FFI bridge for CUE evaluation
//!
//! This crate provides a safe Rust interface to the Go-based CUE evaluator.
//! It handles all FFI operations, memory management, and error handling for
//! calling Go functions from Rust.

mod bridge;
pub mod error;
pub mod frozen;
mod frozen_worker;

pub use bridge::{
    AncestorInstance, CStringPtr, DEFAULT_MODULE_EVAL_TIMEOUT, EVAL_TIMEOUT_ENV_VAR, FieldMeta,
    ImportedInstance, InstanceFailures, InstanceSources, ModuleDependencyVersion,
    ModuleEvalOptions, ModuleResult, PackageScope, SkippedDirectories, SkippedDirectory,
    SkippedReason, SourcesRequest, UnifySource, evaluate_cue_file, evaluate_cue_package,
    evaluate_cue_package_typed, evaluate_module, evaluation_timeout, get_bridge_version,
    instance_sources, instance_sources_with, module_dependency_version,
};
pub use error::{CueEngineError, FrozenWorkerFailure, Result};
