//! Dagger 1.0 workspace sync (`cuenv sync ci --provider dagger`).

use std::collections::HashMap;
use std::path::Path;

use cuenv_core::Result;

use super::implementation::{CiSyncOptions, ProjectInfo};

pub(super) struct DaggerSyncRequest<'request> {
    pub(super) repo_root: &'request Path,
    pub(super) options: CiSyncOptions<'request>,
    pub(super) projects: &'request [ProjectInfo],
}

#[cfg(feature = "dagger")]
pub(super) fn execute_sync_dagger(request: &DaggerSyncRequest<'_>) -> Result<String> {
    use cuenv_ci::compiler::{Compiler, CompilerOptions};
    use cuenv_dagger::{DaggerCiConfig, DaggerWorkspaceEmitter, WorkspaceEmitInput};

    let mut pipelines = Vec::new();
    let mut services = HashMap::new();
    let mut dagger_config = DaggerCiConfig::default();

    for project in request.projects {
        services.extend(project.config.services.clone());

        let Some(ci) = &project.config.ci else {
            continue;
        };

        if let Some(raw_cfg) = ci.provider.as_ref().and_then(|p| p.get("dagger")) {
            dagger_config = serde_json::from_value(raw_cfg.clone()).map_err(|err| {
                cuenv_core::Error::configuration(format!(
                    "Invalid ci.provider.dagger configuration: {err}"
                ))
            })?;
        }

        let mut pipeline_names: Vec<&String> = ci.pipelines.keys().collect();
        pipeline_names.sort();

        for name in pipeline_names {
            let effective = ci.providers_for_pipeline(name);
            if !effective.iter().any(|p| p == "dagger")
                && request.options.provider != Some("dagger")
            {
                continue;
            }
            let pipeline = ci.pipelines.get(name).cloned();
            let compiler = Compiler::with_options(
                project.config.clone(),
                CompilerOptions {
                    project_root: Some(project.project_path.clone()),
                    pipeline_name: Some(name.clone()),
                    pipeline,
                    ci_mode: true,
                    ..Default::default()
                },
            );
            let ir = compiler.compile().map_err(|err| {
                cuenv_core::Error::configuration(format!(
                    "Failed to compile pipeline '{name}' for Dagger sync: {err}"
                ))
            })?;
            pipelines.push(ir);
        }
    }

    if pipelines.is_empty() {
        return Ok(String::new());
    }

    let emitter = DaggerWorkspaceEmitter::new(dagger_config);
    let files = emitter
        .emit_workspace(WorkspaceEmitInput {
            pipelines: &pipelines,
            services: &services,
        })
        .map_err(|err| {
            cuenv_core::Error::configuration(format!("Dagger workspace emitter failed: {err}"))
        })?;

    let mut statuses = Vec::new();
    let mut out_of_sync = Vec::new();

    for file in files {
        let full_path = request.repo_root.join(&file.relative_path);
        let rel_display = file.relative_path.display();
        let exists = full_path.exists();

        if request.options.check {
            if exists {
                let existing = std::fs::read_to_string(&full_path).unwrap_or_default();
                if existing == file.content {
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
            if existing == file.content {
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
        std::fs::write(&full_path, &file.content).map_err(|e| {
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
pub(super) fn execute_sync_dagger(_request: &DaggerSyncRequest<'_>) -> Result<String> {
    Err(cuenv_core::Error::configuration(
        "Dagger support is not enabled. Rebuild with --features dagger",
    ))
}
