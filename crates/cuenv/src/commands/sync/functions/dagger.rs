//! Dagger 1.0 workspace sync (`cuenv sync ci --provider dagger`).

use std::path::Path;

use cuenv_core::Result;

use super::implementation::{CiSyncOptions, ProjectInfo};

pub(super) struct DaggerSyncRequest<'request> {
    pub(super) repo_root: &'request Path,
    pub(super) options: CiSyncOptions<'request>,
    pub(super) explicit_path: Option<&'request Path>,
    pub(super) package: &'request str,
    pub(super) projects: &'request [ProjectInfo],
}

#[cfg(feature = "dagger")]
pub(super) fn execute_sync_dagger(request: &DaggerSyncRequest<'_>) -> Result<String> {
    use crate::commands::dagger_generation::{
        DaggerGeneration, DaggerProject, PipelineSelection, compile_modules, repository_root,
    };
    use std::collections::BTreeMap;
    use std::collections::btree_map::Entry;

    let projects = request
        .projects
        .iter()
        .map(|project| DaggerProject {
            project: &project.config,
            project_dir: &project.project_path,
            module_dir: &project.module_root,
        })
        .collect();
    let repository = repository_root(request.repo_root);
    let modules = compile_modules(
        projects,
        &DaggerGeneration {
            package: request.package,
            repository: &repository,
            pipeline: None,
            selection: if request.options.provider == Some("dagger") {
                request.explicit_path.map_or(
                    PipelineSelection::Explicit,
                    PipelineSelection::ExplicitProject,
                )
            } else {
                PipelineSelection::DaggerProvider
            },
        },
    )?;
    let mut files = BTreeMap::new();
    for module in modules {
        for file in module.emit_workspace(request.package)? {
            let workspace_config = file
                .relative_path
                .file_name()
                .is_some_and(|name| name == "dagger.toml");
            match files.entry(file.relative_path) {
                Entry::Vacant(entry) => {
                    entry.insert(file.content);
                }
                Entry::Occupied(mut entry) if workspace_config => {
                    let module_config = file.content.lines().skip(1).collect::<Vec<_>>().join("\n");
                    let content: &mut String = entry.get_mut();
                    content.push_str(&module_config);
                    content.push('\n');
                }
                Entry::Occupied(entry) => {
                    return Err(cuenv_core::Error::configuration(format!(
                        "Dagger modules generate the same file '{}'; configure distinct modulePath values.",
                        entry.key().display()
                    )));
                }
            }
        }
    }

    for relative_path in files.keys() {
        validate_destination(&repository, relative_path)?;
    }

    let mut statuses = Vec::new();
    let mut out_of_sync = Vec::new();

    for (relative_path, content) in files {
        let full_path = repository.join(&relative_path);
        let rel_display = relative_path.display();
        let exists = full_path.exists();

        if request.options.check {
            if exists {
                let existing = std::fs::read_to_string(&full_path).unwrap_or_default();
                if existing == content {
                    statuses.push(format!("Dagger: {rel_display} in sync"));
                } else {
                    out_of_sync.push(format!("{rel_display} (out of sync)"));
                }
            } else {
                out_of_sync.push(format!("{rel_display} (missing)"));
            }
            continue;
        }

        if exists && !request.options.dry_run.is_dry_run() {
            let existing = std::fs::read_to_string(&full_path).unwrap_or_default();
            if existing == content {
                statuses.push(format!("Dagger: {rel_display} (unchanged)"));
                continue;
            }
        }

        if request.options.dry_run.is_dry_run() {
            if exists {
                statuses.push(format!("Dagger: Would update {rel_display}"));
            } else {
                statuses.push(format!("Dagger: Would create {rel_display}"));
            }
            continue;
        }

        if let Some(parent) = full_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                cuenv_core::Error::io_with_path("create directory", parent.to_path_buf(), e)
            })?;
        }
        std::fs::write(&full_path, &content).map_err(|e| {
            cuenv_core::Error::io_with_path("write Dagger workspace file", full_path.clone(), e)
        })?;

        if exists {
            statuses.push(format!("Dagger: Updated {rel_display}"));
        } else {
            statuses.push(format!("Dagger: Created {rel_display}"));
        }
    }

    if !out_of_sync.is_empty() {
        return Err(cuenv_core::Error::configuration(format!(
            "Dagger workspace files out of sync: {}. Run 'cuenv sync ci --provider dagger' to update.",
            out_of_sync.join(", ")
        )));
    }

    Ok(statuses.join("\n"))
}

#[cfg(not(feature = "dagger"))]
pub(super) fn execute_sync_dagger(request: &DaggerSyncRequest<'_>) -> Result<String> {
    let _ = (
        request.repo_root,
        request.options,
        request.explicit_path,
        request.package,
        request.projects,
    );
    Err(cuenv_core::Error::configuration(
        "Dagger support is not enabled. Rebuild with --features dagger",
    ))
}

#[cfg(feature = "dagger")]
fn validate_destination(root: &Path, relative: &Path) -> Result<()> {
    let mut path = root.to_path_buf();
    for component in relative.components() {
        path.push(component);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_symlink() => {
                return Err(cuenv_core::Error::configuration(format!(
                    "Dagger sync will not write through symbolic link '{}'",
                    path.display()
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(cuenv_core::Error::io_with_path(
                    "inspect Dagger output path",
                    path,
                    error,
                ));
            }
        }
    }
    let old = std::fs::read_to_string(&path).map_err(|error| {
        cuenv_core::Error::io_with_path("read Dagger output file", path.clone(), error)
    })?;
    if !old.starts_with("# Code generated by cuenv. DO NOT EDIT.\n") {
        return Err(cuenv_core::Error::configuration(format!(
            "Dagger sync cannot overwrite manually maintained '{}'. Use 'cuenv ci --export dagger' to generate the module source separately.",
            path.display()
        )));
    }
    Ok(())
}
