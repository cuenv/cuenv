//! Location of a project's separately evaluated infrastructure package.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::cli::CliError;
use crate::commands::env_file::find_cue_module_root;

/// A resolved infrastructure package associated with one project directory.
#[derive(Debug, Clone)]
pub struct Locator {
    pub(crate) directory: PathBuf,
    pub(crate) package: String,
    pub(crate) project_package: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Configuration {
    directory: String,
    package: String,
    #[serde(default = "default_project_package")]
    project_package: String,
}

fn default_project_package() -> String {
    "cuenv".to_string()
}

/// Load `.infrastructure.cue` from the exact project directory, if present.
pub fn load(project: &Path) -> Result<Option<Locator>, CliError> {
    let file = project.join(".infrastructure.cue");
    if !file.exists() {
        return Ok(None);
    }
    let value = cuengine::evaluate_cue_file(&file).map_err(|error| {
        CliError::config(format!("cannot evaluate {}: {error}", file.display()))
    })?;
    let configuration: Configuration = serde_json::from_value(value)
        .map_err(|error| CliError::config(format!("invalid {}: {error}", file.display())))?;
    if configuration.directory.trim().is_empty()
        || configuration.package.trim().is_empty()
        || configuration.project_package.trim().is_empty()
    {
        return Err(CliError::config(format!(
            "{} must specify a nonempty directory and package",
            file.display()
        )));
    }
    let directory = project
        .join(&configuration.directory)
        .canonicalize()
        .map_err(|error| {
            CliError::config(format!(
                "cannot resolve infrastructure directory in {}: {error}",
                file.display()
            ))
        })?;
    if !directory.is_dir() {
        return Err(CliError::config(format!(
            "{} is not a directory",
            directory.display()
        )));
    }
    let project_module = find_cue_module_root(project);
    let infrastructure_module = find_cue_module_root(&directory);
    if project_module.is_none() || project_module != infrastructure_module {
        return Err(CliError::config(format!(
            "infrastructure directory {} must be in the same CUE module as {}",
            directory.display(),
            project.display()
        )));
    }
    Ok(Some(Locator {
        directory,
        package: configuration.package,
        project_package: configuration.project_package,
    }))
}
