//! Module-wide CUE evaluation types.

mod deserialize;
mod module_evaluation;
mod task_refs;
mod task_sources;
mod unique_names;

pub use module_evaluation::{
    DirectReference, Instance, InstanceKind, ModuleEvaluation, ModuleEvaluationInput,
    ModuleEvaluationMetadata, ModuleEvaluationOptions, ReferenceMap, SourceMap, TASK_FIELD,
};
pub use unique_names::{
    DuplicateProjectName, NamedProject, duplicate_names_error, duplicate_project_names,
};
