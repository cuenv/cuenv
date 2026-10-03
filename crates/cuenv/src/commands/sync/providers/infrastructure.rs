//! Infrastructure sync provider: regenerates the CUE types of every
//! infrastructure provider `cuenv.lock` pins.
//!
//! It reads only `cuenv.lock`, never the project's CUE, so it can restore
//! generated packages the project imports but that are missing. Each pinned
//! release is installed (requiring the pinned archive), its schema must still
//! have the pinned digest, and its types are rendered into `cue.mod/gen`.

use async_trait::async_trait;
use cuenv_core::Result;
use cuenv_infrastructure::registry::ProviderSource;

use crate::cli::CliError;
use crate::commands::infrastructure::types::{self, Generation, Module, pinned_archives};
use crate::commands::sync::provider::{SyncMode, SyncProvider, SyncRequest, SyncResult};

/// Sync provider for generated infrastructure provider types.
pub struct InfrastructureSyncProvider;

#[async_trait]
impl SyncProvider for InfrastructureSyncProvider {
    fn name(&self) -> &'static str {
        "infrastructure"
    }

    // The lockfile belongs to the CUE module, so path and workspace scope
    // are the same.
    async fn sync(&self, request: SyncRequest<'_>) -> Result<SyncResult> {
        sync_module(request)
            .await
            .map_err(|error| cli_error(&error))
    }
}

fn cli_error(error: &CliError) -> cuenv_core::Error {
    match error.help() {
        Some(help) => cuenv_core::Error::configuration(format!("{}\n{help}", error.message())),
        None => cuenv_core::Error::configuration(error.message().to_string()),
    }
}

async fn sync_module(request: SyncRequest<'_>) -> std::result::Result<SyncResult, CliError> {
    let module = Module::containing(request.path)?;
    let lockfile = module.lockfile()?;
    let mut lines = Vec::new();
    let mut out_of_date = Vec::new();
    let patterns = types::gitignore_patterns(&lockfile)?;
    if types::sync_gitignore(&module, &patterns, &request.options.mode)? {
        match request.options.mode {
            SyncMode::Write => lines.push(".gitignore: updated".to_string()),
            SyncMode::DryRun => lines.push("Would update .gitignore".to_string()),
            SyncMode::Check => {
                out_of_date.push(".gitignore section `cuenv infrastructure`".to_string());
            }
        }
    }
    if lockfile.infrastructure_providers.is_empty() {
        lines.push("No infrastructure providers are pinned in cuenv.lock.".to_string());
    }
    for (source_text, pin) in &lockfile.infrastructure_providers {
        let source = ProviderSource::parse(source_text)
            .map_err(|error| types::infrastructure_error(&error))?;
        let platforms = pinned_archives(pin);
        let generated = Generation {
            source: &source,
            version: &pin.version,
            platforms: &platforms,
        }
        .run()
        .await?;
        lines.extend(
            generated
                .warnings
                .iter()
                .map(|warning| format!("warning: {source}: {warning}")),
        );
        let generated = generated.types;
        if generated.schema_digest != pin.schema_digest {
            return Err(CliError::config_with_help(
                format!(
                    "{source} {} reports schema {}, but cuenv.lock pins {}",
                    pin.version, generated.schema_digest, pin.schema_digest
                ),
                format!(
                    "The installed binary is not the release that was pinned. Run `cuenv \
                     infrastructure provider add {source}@{}` to pin it again.",
                    pin.version
                ),
            ));
        }
        let differences = types::differences(&module, &generated)?;
        if differences.is_empty() {
            lines.push(format!("{source} {}: up to date", pin.version));
            continue;
        }
        let summary = format!(
            "{source} {}: {} file(s) to write, {} to remove",
            pin.version,
            differences.changed.len(),
            differences.removed.len()
        );
        match request.options.mode {
            SyncMode::Write => {
                types::write(&module, &generated)?;
                lines.push(format!(
                    "{source} {}: wrote {}",
                    pin.version,
                    generated.directory.display()
                ));
            }
            SyncMode::DryRun => lines.push(format!("Would update {summary}")),
            SyncMode::Check => {
                out_of_date.push(summary);
                for path in differences.changed.iter().chain(&differences.removed) {
                    out_of_date.push(format!("  {}", path.display()));
                }
            }
        }
    }
    if !out_of_date.is_empty() {
        return Err(CliError::config_with_help(
            format!(
                "Generated infrastructure provider types are out of date:\n{}",
                out_of_date.join("\n")
            ),
            "Run `cuenv sync infrastructure` (or `cuenv sync`) to regenerate them from cuenv.lock.",
        ));
    }
    Ok(SyncResult::success(lines.join("\n")))
}
