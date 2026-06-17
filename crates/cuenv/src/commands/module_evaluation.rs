use super::{CommandExecutor, convert_engine_error, env_file, schema_compat};
use crate::commands::module_utils::EvaluationMetadataBuilder;
use cuengine::ModuleEvalOptions;
use cuenv_core::cue::discovery::{adjust_meta_key_path, compute_relative_path, format_eval_errors};
use cuenv_core::{ModuleEvaluation, ModuleEvaluationInput, Result};
use rayon::prelude::*;
use std::collections::HashMap;
use std::path::Path;

impl CommandExecutor {
    pub(super) fn evaluate_path_module(&self, target_path: &Path) -> Result<ModuleEvaluation> {
        let module_root = env_file::find_cue_module_root(target_path).ok_or_else(|| {
            cuenv_core::Error::configuration(format!(
                "No CUE module found (looking for cue.mod/) starting from: {}",
                target_path.display()
            ))
        })?;
        schema_compat::warn_for_module(&module_root)?;

        let target_rel_path = compute_relative_path(target_path, &module_root);
        let options = ModuleEvalOptions {
            recursive: false,
            with_meta: true,
            with_references: true,
            target_dir: Some(target_path.to_string_lossy().to_string()),
            ..Default::default()
        };

        let raw = cuengine::evaluate_module(&module_root, &self.package, Some(&options))
            .map_err(convert_engine_error)?;

        let mut instances = HashMap::new();
        let mut projects = Vec::new();
        let mut metadata = EvaluationMetadataBuilder::default();

        for (path_str, value) in raw.instances {
            let rel_path = if path_str == "." {
                target_rel_path.clone()
            } else {
                path_str
            };
            instances.insert(rel_path, value);
        }

        for project_path in raw.projects {
            let rel_project_path = if project_path == "." {
                target_rel_path.clone()
            } else {
                project_path
            };
            if !projects.contains(&rel_project_path) {
                projects.push(rel_project_path);
            }
        }

        for (meta_key, meta_value) in raw.meta {
            let adjusted_key = adjust_meta_key_path(&meta_key, &target_rel_path);
            metadata.insert(adjusted_key, meta_value);
        }

        Ok(ModuleEvaluation::from_raw_parts(ModuleEvaluationInput {
            root: module_root,
            raw_instances: instances,
            project_paths: projects,
            metadata: metadata.finish(),
        }))
    }

    pub(super) fn evaluate_workspace_module(&self, module_root: &Path) -> Result<ModuleEvaluation> {
        // Fast path: evaluate the entire module in a single recursive CUE
        // evaluation (equivalent to `cue eval ./...`). This loads the module
        // and compiles imported schema packages once instead of once per
        // directory, which is significantly faster for monorepos.
        match self.evaluate_workspace_module_recursive(module_root) {
            Ok(module) => Ok(module),
            Err(e) => {
                tracing::warn!(
                    module_root = %module_root.display(),
                    error = %e,
                    "Recursive workspace evaluation failed - falling back to per-directory evaluation"
                );
                self.evaluate_workspace_module_fan_out(module_root)
            }
        }
    }

    /// Evaluate the whole workspace with one recursive CUE evaluation.
    ///
    /// The Go bridge loads the module once and builds every package instance
    /// in a shared CUE context, skipping directories whose package does not
    /// match or that fail to build (matching the per-directory fan-out
    /// semantics).
    fn evaluate_workspace_module_recursive(&self, module_root: &Path) -> Result<ModuleEvaluation> {
        let options = ModuleEvalOptions {
            recursive: true,
            with_meta: true,
            with_references: true,
            ..Default::default()
        };

        tracing::info!("evaluate_workspace_module single recursive evaluation");
        let raw = cuengine::evaluate_module(module_root, &self.package, Some(&options))
            .map_err(convert_engine_error)?;

        let mut metadata = EvaluationMetadataBuilder::default();
        for (meta_key, meta_value) in raw.meta {
            metadata.insert(meta_key, meta_value);
        }

        Ok(ModuleEvaluation::from_raw_parts(ModuleEvaluationInput {
            root: module_root.to_path_buf(),
            raw_instances: raw.instances,
            project_paths: raw.projects,
            metadata: metadata.finish(),
        }))
    }

    fn evaluate_workspace_module_fan_out(&self, module_root: &Path) -> Result<ModuleEvaluation> {
        let env_cue_dirs =
            cuenv_core::cue::discovery::discover_env_cue_directories(module_root, &self.package);

        if env_cue_dirs.is_empty() {
            return Err(cuenv_core::Error::configuration(format!(
                "No env.cue files declaring package '{}' found in module: {}",
                self.package,
                module_root.display(),
            )));
        }

        let package = &self.package;
        tracing::info!(
            env_cue_dirs = env_cue_dirs.len(),
            rayon_threads = rayon::current_num_threads(),
            "evaluate_workspace_module fan-out"
        );
        let results: Vec<_> = env_cue_dirs
            .par_iter()
            .map(|dir| {
                tracing::debug!(dir = %dir.display(), "cuengine::evaluate_module begin");
                let options = ModuleEvalOptions {
                    recursive: false,
                    with_meta: true,
                    with_references: true,
                    target_dir: Some(dir.to_string_lossy().to_string()),
                    ..Default::default()
                };
                let dir_rel_path = compute_relative_path(dir, module_root);

                match cuengine::evaluate_module(module_root, package, Some(&options)) {
                    Ok(raw) => Ok((dir_rel_path, raw)),
                    Err(e) => {
                        tracing::warn!(
                            dir = %dir.display(),
                            error = %e,
                            "Failed to evaluate env.cue - skipping directory"
                        );
                        Err((dir.clone(), e))
                    }
                }
            })
            .collect();

        let mut all_instances = HashMap::new();
        let mut all_projects = Vec::new();
        let mut metadata = EvaluationMetadataBuilder::default();
        let mut eval_errors = Vec::new();

        for result in results {
            match result {
                Ok((dir_rel_path, raw)) => {
                    for (path_str, value) in raw.instances {
                        let rel_path = if path_str == "." {
                            dir_rel_path.clone()
                        } else {
                            path_str
                        };
                        all_instances.insert(rel_path, value);
                    }

                    for project_path in raw.projects {
                        let rel_project_path = if project_path == "." {
                            dir_rel_path.clone()
                        } else {
                            project_path
                        };
                        if !all_projects.contains(&rel_project_path) {
                            all_projects.push(rel_project_path);
                        }
                    }

                    for (meta_key, meta_value) in raw.meta {
                        let adjusted_key = adjust_meta_key_path(&meta_key, &dir_rel_path);
                        metadata.insert(adjusted_key, meta_value);
                    }
                }
                Err((dir, e)) => eval_errors.push((dir, e)),
            }
        }

        if all_instances.is_empty() {
            let error_summary = format_eval_errors(&eval_errors);
            return Err(cuenv_core::Error::configuration(format!(
                "No instances could be evaluated. All directories failed:\n{error_summary}"
            )));
        }

        Ok(ModuleEvaluation::from_raw_parts(ModuleEvaluationInput {
            root: module_root.to_path_buf(),
            raw_instances: all_instances,
            project_paths: all_projects,
            metadata: metadata.finish(),
        }))
    }
}
