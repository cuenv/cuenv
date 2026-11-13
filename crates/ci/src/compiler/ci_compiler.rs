//! CI compiler entry point and state.

use crate::ir::{IntermediateRepresentation, IrValidator};
use cuenv_core::ci::PipelineTask;
use cuenv_core::manifest::Project;

use super::{CompilerError, CompilerOptions, FileAnchors};
use cuenv_core::module::ImportedProjects;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Compiler for transforming cuenv tasks to IR
pub struct Compiler {
    /// Project being compiled
    pub(super) project: Project,

    /// Compiler options
    pub(super) options: CompilerOptions,

    /// The packages the project imports, read the first time a file
    /// reference is bound (see [`CompilerOptions::file_anchors`]).
    pub(super) imports: OnceLock<Result<ImportedProjects, String>>,

    /// The packages each other project a pipeline reaches imports, by its
    /// directory relative to the module root, read the first time a
    /// reference of it is bound.
    pub(super) foreign_imports: Mutex<BTreeMap<PathBuf, Result<ImportedProjects, String>>>,
}

impl Compiler {
    /// Create a new compiler for the given project
    #[must_use]
    pub fn new(project: Project) -> Self {
        Self::with_options(project, CompilerOptions::default())
    }

    /// Create a compiler with custom options
    #[must_use]
    pub const fn with_options(project: Project, options: CompilerOptions) -> Self {
        Self {
            project,
            options,
            imports: OnceLock::new(),
            foreign_imports: Mutex::new(BTreeMap::new()),
        }
    }

    /// The directory, relative to the module root, of the project called
    /// `name`, as the project in `from` (relative to the module root; `None`
    /// for the project being compiled) means it: among what that project
    /// imports.
    pub(super) fn bind_project(&self, from: Option<&Path>, name: &str) -> Result<PathBuf, String> {
        let Some(from) = from else {
            return self.imported_projects()?.bind(name);
        };
        let module_root = self.options.module_root.as_ref().ok_or_else(|| {
            "this compilation does not know where the module lies, so it cannot read which \
             projects another project imports"
                .to_string()
        })?;
        let mut known = self
            .foreign_imports
            .lock()
            .map_err(|_| "the imports of other projects are unavailable".to_string())?;
        let imported = known.entry(from.to_path_buf()).or_insert_with(|| {
            crate::imports::imported_projects(
                module_root,
                &module_root.join(from),
                crate::imports::PROJECT_PACKAGE,
            )
        });
        imported.as_ref().map_err(Clone::clone)?.bind(name)
    }

    /// The packages the project imports, where its file references bind.
    pub(super) fn imported_projects(&self) -> Result<&ImportedProjects, String> {
        const PACKAGE: &str = crate::imports::PROJECT_PACKAGE;
        self.imports
            .get_or_init(|| match &self.options.file_anchors {
                Some(FileAnchors::Known(imported)) => Ok(imported.clone()),
                Some(FileAnchors::Instance {
                    module_root,
                    directory,
                }) => crate::imports::imported_projects(module_root, directory, PACKAGE),
                None => {
                    let module_root = self.options.module_root.as_ref().ok_or_else(|| {
                        "this compilation does not know where the project lies, so it cannot \
                         read which projects it imports"
                            .to_string()
                    })?;
                    let directory = match self.options.project_path.as_deref() {
                        Some(path) if !path.is_empty() && path != "." => module_root.join(path),
                        _ => module_root.clone(),
                    };
                    crate::imports::imported_projects(module_root, &directory, PACKAGE)
                }
            })
            .as_ref()
            .map_err(Clone::clone)
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

        // Everything the pipeline reaches must be able to run, with its file
        // references bound; trigger conditions are derived from the same
        // walk.
        if let Some(ref pipeline) = self.options.pipeline {
            let inputs = self.reached_inputs(pipeline)?;
            if self.project.ci.is_some() {
                ir.pipeline.trigger = Some(self.build_trigger_condition(pipeline, &inputs));
            }
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
