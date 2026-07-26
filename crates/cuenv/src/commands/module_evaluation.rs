use super::{CommandExecutor, convert_engine_error, env_file, schema_compat};
use crate::commands::module_utils::EvaluationMetadataBuilder;
use cuengine::ModuleEvalOptions;
use cuenv_core::cue::discovery::{adjust_meta_key_path, compute_relative_path};
use cuenv_core::{ModuleEvaluation, ModuleEvaluationInput, Result};
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
        // Workspace operations evaluate the selected CUE package exactly once
        // across the whole module. Discovery belongs to CUE, so it must not
        // depend on a conventional filename or retry directories independently.
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;
    use std::fs;
    use tempfile::TempDir;
    use tokio::sync::mpsc;

    type TestResult<T = ()> = std::result::Result<T, Box<dyn Error>>;

    fn temp_module() -> TestResult<TempDir> {
        Ok(tempfile::Builder::new()
            .prefix("cuenv-recursive-consumer-")
            .tempdir()?)
    }

    fn write_module(root: &Path) -> TestResult {
        fs::create_dir_all(root.join("cue.mod"))?;
        fs::write(
            root.join("cue.mod/module.cue"),
            "module: \"example.com/recursive-consumer-test\"\nlanguage: {version: \"v0.9.0\"}\n",
        )?;
        Ok(())
    }

    fn executor() -> CommandExecutor {
        let (sender, _receiver) = mpsc::unbounded_channel();
        CommandExecutor::new(sender, "cuenv".to_string())
    }

    #[test]
    fn workspace_evaluation_discovers_arbitrary_cue_filenames() -> TestResult {
        let temp = temp_module()?;
        write_module(temp.path())?;
        fs::write(
            temp.path().join("defaults.cue"),
            "package cuenv\nenv: {ROOT: \"yes\"}\n",
        )?;
        fs::create_dir_all(temp.path().join("nested"))?;
        fs::write(
            temp.path().join("nested/service.cue"),
            "package cuenv\nname: \"nested\"\n",
        )?;
        fs::create_dir_all(temp.path().join("worker"))?;
        fs::write(
            temp.path().join("worker/configuration.cue"),
            "package cuenv\nname: \"worker\"\n",
        )?;

        let module = executor().evaluate_workspace_module(temp.path())?;
        let mut names = module
            .projects()
            .filter_map(|instance| instance.project_name().map(ToOwned::to_owned))
            .collect::<Vec<_>>();
        names.sort();

        assert_eq!(names, ["nested", "worker"]);

        Ok(())
    }

    #[test]
    fn workspace_evaluation_propagates_a_selected_package_failure() -> TestResult {
        let temp = temp_module()?;
        write_module(temp.path())?;
        fs::write(
            temp.path().join("defaults.cue"),
            "package cuenv\nenv: {ROOT: \"yes\"}\n",
        )?;
        fs::create_dir_all(temp.path().join("valid"))?;
        fs::write(
            temp.path().join("valid/project.cue"),
            "package cuenv\nname: \"valid\"\n",
        )?;
        fs::create_dir_all(temp.path().join("broken"))?;
        fs::write(
            temp.path().join("broken/config.cue"),
            "package cuenv\nname: \"broken\"\nimpossible: 1 & 2\n",
        )?;

        let error = executor()
            .evaluate_workspace_module(temp.path())
            .expect_err("a broken selected-package instance must fail the workspace evaluation");
        assert!(error.to_string().contains("broken"), "{error}");

        Ok(())
    }
}
