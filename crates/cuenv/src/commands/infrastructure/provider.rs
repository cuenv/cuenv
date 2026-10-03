//! `cuenv infrastructure provider add|remove`.
//!
//! `add` installs a provider release, renders CUE types from its schema into
//! the CUE module's `cue.mod/gen` and pins version, schema digest and every
//! platform's archive SHA-256 in `cuenv.lock`. It never evaluates or edits
//! the project's CUE: the generated packages may not be imported yet, and the
//! printed snippet shows how to use them. `remove` undoes both.

use std::path::Path;

use cuenv_core::lockfile::{LockedInfrastructureProvider, Lockfile};
use cuenv_events::{emit_stderr, emit_stdout};
use cuenv_infrastructure::cue_types::{self, GeneratedTypes};
use cuenv_infrastructure::registry::{
    ProviderInstaller, ProviderSource, default_cache_directory, validate_version,
};

use super::types::{self, Generation, Module, infrastructure_error};
use crate::cli::CliError;

/// What `cuenv infrastructure provider` should do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderAction {
    /// Generate types for `<source>@<version>` and pin it.
    Add {
        /// `<source>@<version>`, such as `hashicorp/random@3.9.1`.
        release: String,
    },
    /// Delete a provider's generated types and its pin.
    Remove {
        /// Provider source address, such as `hashicorp/random`.
        source: String,
    },
}

/// Run `cuenv infrastructure provider` for the CUE module containing `path`.
///
/// # Errors
///
/// Returns an error when the path is not in a CUE module, the release cannot
/// be installed or its schema read, or files cannot be written.
pub async fn execute_provider(path: &str, action: &ProviderAction) -> Result<(), CliError> {
    let module = Module::containing(Path::new(path))?;
    match action {
        ProviderAction::Add { release } => add(&module, release).await,
        ProviderAction::Remove { source } => remove(&module, source),
    }
}

/// Split `<source>@<version>`.
fn parse_release(release: &str) -> Result<(ProviderSource, String), CliError> {
    let (source, version) = release.rsplit_once('@').ok_or_else(|| {
        CliError::config_with_help(
            format!("'{release}' names no version"),
            "Give an exact version: `cuenv infrastructure provider add hashicorp/random@3.9.1`.",
        )
    })?;
    let source = ProviderSource::parse(source).map_err(|error| infrastructure_error(&error))?;
    validate_version(version).map_err(|error| infrastructure_error(&error))?;
    // Refuse sources that cannot be import paths before downloading.
    cue_types::import_path(&source).map_err(|error| infrastructure_error(&error))?;
    Ok((source, version.to_string()))
}

async fn add(module: &Module, release: &str) -> Result<(), CliError> {
    let (source, version) = parse_release(release)?;
    let mut lockfile = module.lockfile()?;
    let installer = ProviderInstaller::new(default_cache_directory())
        .map_err(|error| infrastructure_error(&error))?;
    let platforms = installer
        .archive_checksums(&source, &version)
        .await
        .map_err(|error| infrastructure_error(&error))?;
    let generated = Generation {
        source: &source,
        version: &version,
        platforms: &platforms,
    }
    .run()
    .await?;
    for warning in &generated.warnings {
        emit_stderr!(format!("warning: {warning}"));
    }
    let types = generated.types;
    types::write(module, &types)?;
    lockfile
        .upsert_infrastructure_provider(
            source.to_string(),
            LockedInfrastructureProvider {
                version: version.clone(),
                schema_digest: types.schema_digest.clone(),
                platforms: platforms
                    .iter()
                    .map(|(platform, hex)| (platform.clone(), format!("sha256:{hex}")))
                    .collect(),
            },
        )
        .map_err(|error| CliError::config(error.to_string()))?;
    lockfile
        .save(&module.lockfile_path())
        .map_err(|error| CliError::config(error.to_string()))?;
    emit_stdout!(added_message(&types, &version));
    Ok(())
}

fn remove(module: &Module, source: &str) -> Result<(), CliError> {
    let source = ProviderSource::parse(source).map_err(|error| infrastructure_error(&error))?;
    let mut lockfile = module.lockfile()?;
    let pinned = lockfile
        .remove_infrastructure_provider(&source.to_string())
        .is_some();
    let removed = types::remove(module, &source)?;
    if !pinned && !removed {
        return Err(CliError::config(format!(
            "{source} has neither generated types nor a pin in {}",
            module.lockfile_path().display()
        )));
    }
    if pinned {
        if lockfile == Lockfile::default() {
            // Nothing else was locked; leave no empty lockfile behind.
            std::fs::remove_file(module.lockfile_path()).map_err(|error| {
                CliError::config(format!(
                    "cannot remove {}: {error}",
                    module.lockfile_path().display()
                ))
            })?;
        } else {
            lockfile
                .save(&module.lockfile_path())
                .map_err(|error| CliError::config(error.to_string()))?;
        }
    }
    emit_stdout!(format!(
        "Removed {source}: {}{}{}. Remove its imports and `providers` entry from your CUE.",
        if removed { "generated types" } else { "" },
        if removed && pinned { " and " } else { "" },
        if pinned { "cuenv.lock pin" } else { "" },
    ));
    Ok(())
}

/// What `add` prints: where the types went and how to use them.
fn added_message(types: &GeneratedTypes, version: &str) -> String {
    let example = types
        .resource_types
        .first()
        .map_or("example_resource", String::as_str);
    let provider_alias = &types.package_name;
    let local_name = example.split('_').next().unwrap_or(example);
    format!(
        "Generated CUE types for {import} {version} ({count} resource types) in {directory}\n\
         and pinned it in cuenv.lock.\n\
         \n\
         Import the provider and the resource types you use:\n\
         \n\
         import (\n\
         \t{provider_import}\n\
         \t{resource_import}\n\
         )\n\
         \n\
         infrastructure: {{\n\
         \tproviders: {local_name}: {provider_alias}.#Provider\n\
         \tresources: example: {example}.#Resource & {{\n\
         \t\tconfiguration: {{\n\
         \t\t\t// arguments of {example}\n\
         \t\t}}\n\
         \t}}\n\
         }}\n\
         \n\
         Commit {directory} and cuenv.lock. Regenerate with `cuenv sync infrastructure`.",
        import = types.import_path,
        count = types.resource_types.len(),
        directory = types.directory.display(),
        provider_import = types.provider_import(),
        resource_import = types.resource_import(example),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn releases_need_an_exact_version() {
        let (source, version) = parse_release("hashicorp/random@3.9.1").unwrap();
        assert_eq!(source.to_string(), "registry.terraform.io/hashicorp/random");
        assert_eq!(version, "3.9.1");
        let (source, _) = parse_release("registry.opentofu.org/hashicorp/random@3.9.1").unwrap();
        assert_eq!(source.hostname, "registry.opentofu.org");
        for invalid in [
            "hashicorp/random",
            "hashicorp/random@~> 3.9",
            "hashicorp/random@",
            "random@3.9.1",
            "localhost:8443/hashicorp/random@3.9.1",
        ] {
            assert!(parse_release(invalid).is_err(), "{invalid}");
        }
    }
}
