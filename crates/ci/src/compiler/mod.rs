//! Compiler from cuenv task definitions to IR v1.5
//!
//! Transforms a cuenv `Project` with tasks into an intermediate representation
//! suitable for emitting orchestrator-native CI configurations.
//!
//! ## Contributors
//!
//! Contributors are defined in CUE (see `contrib/contributors/`). The compiler
//! evaluates the `ci.contributors` array and injects active contributors'
//! tasks into the appropriate build phases.
//!
//! ## Layout
//!
//! [`Compiler`] and its [`Compiler::compile`] entry point stay here because
//! every submodule extends the same type with its own `impl Compiler` block:
//! `purity` (flake analysis), `lowering` (task nodes to IR tasks),
//! `contributors` and `triggers`. [`CompilerOptions`] lives in `options`.

mod contributors;
pub mod digest;
mod lowering;
mod options;
mod purity;
mod triggers;

use crate::ir::{IntermediateRepresentation, IrValidator};
use cuenv_core::ci::PipelineTask;
use cuenv_core::manifest::Project;

pub use crate::error::CompilerError;

pub use options::CompilerOptions;

/// Compiler for transforming cuenv tasks to IR
pub struct Compiler {
    /// Project being compiled
    project: Project,

    /// Compiler options
    options: CompilerOptions,
}

impl Compiler {
    /// Create a new compiler for the given project
    #[must_use]
    pub fn new(project: Project) -> Self {
        Self {
            project,
            options: CompilerOptions::default(),
        }
    }

    /// Create a compiler with custom options
    #[must_use]
    pub const fn with_options(project: Project, options: CompilerOptions) -> Self {
        Self { project, options }
    }

    /// Compile project tasks to IR
    ///
    /// # Errors
    ///
    /// Returns `CompilerError` if task compilation fails.
    pub fn compile(&self) -> Result<IntermediateRepresentation, CompilerError> {
        let mut ir = IntermediateRepresentation::new(&self.project.name);

        // Set pipeline context from options (enables environment-aware contributors)
        if let Some(ref pipeline) = self.options.pipeline {
            ir.pipeline.environment.clone_from(&pipeline.environment);
            ir.pipeline.pipeline_tasks = pipeline
                .tasks
                .iter()
                .map(PipelineTask::task_name)
                .map(String::from)
                .collect();
            // Preserve full pipeline task definitions (including matrix configs)
            ir.pipeline.pipeline_task_defs.clone_from(&pipeline.tasks);
        }

        // Set up trigger conditions from CI configuration using the pipeline from options
        if let Some(ref pipeline) = self.options.pipeline
            && let Some(ci_config) = &self.project.ci
        {
            ir.pipeline.trigger = Some(self.build_trigger_condition(pipeline, ci_config));
        }

        // Compile tasks
        self.compile_tasks(&self.project.tasks, &mut ir)?;

        // Fix artifact download paths to use actual upstream task outputs
        // (artifact_downloads are initially created with paths derived from task names,
        // but should use the actual output paths from the upstream tasks)
        Self::fix_artifact_download_paths(&mut ir);

        // Apply CUE-defined contributors
        self.apply_cue_contributors(&mut ir);

        // Validate the IR
        let validator = IrValidator::new(&ir);
        validator.validate().map_err(|errors| {
            let error_messages: Vec<String> = errors
                .iter()
                .map(std::string::ToString::to_string)
                .collect();
            CompilerError::ValidationFailed(error_messages.join(", "))
        })?;

        Ok(ir)
    }
}

#[cfg(test)]
#[path = "compiler_tests.rs"]
mod tests;
