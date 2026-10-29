//! `cuenv infrastructure provider add|remove`.
//!
//! `add` installs a provider release, renders CUE types from its schema into
//! the CUE module's `cue.mod/gen` and pins version, schema digest and every
//! platform's archive SHA-256 in `cuenv.lock`. It never evaluates or edits
//! the project's CUE: the generated packages may not be imported yet, and the
//! printed snippet shows how to use them. `remove` undoes both.

use std::fmt::Write as _;
use std::path::Path;

use cuenv_core::lockfile::{LockedInfrastructureProvider, Lockfile};
use cuenv_events::{emit_stderr, emit_stdout};
use cuenv_infrastructure::cue_types::{self, GeneratedTypes};
use cuenv_infrastructure::registry::{
    ProviderInstaller, ProviderSource, default_cache_directory, validate_version,
};

use super::types::{self, Generation, Module, infrastructure_error};
use crate::cli::CliError;
use crate::commands::sync::SyncMode;

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
    let installer = ProviderInstaller::try_new(default_cache_directory())
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
    types::sync_gitignore(
        module,
        &types::gitignore_patterns(&lockfile)?,
        &SyncMode::Write,
    )?;
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
    types::sync_gitignore(
        module,
        &types::gitignore_patterns(&lockfile)?,
        &SyncMode::Write,
    )?;
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
    let provider_alias = &types.package_name;
    let local_name = provider_alias.trim_end_matches("_provider");
    let mut imports = format!("\t{}\n", types.provider_import());
    let mut declarations = String::new();
    if let Some(example) = types.resource_types.first() {
        let _ = writeln!(
            imports,
            "\tresource_type {}",
            types.resource_import(example)
        );
        let _ = writeln!(
            declarations,
            "\tresources: example: resource_type.#Resource & {{\n\
             \t\tprovider: \"{local_name}\"\n\
             \t\tconfiguration: {{\n\
             \t\t\t// arguments of {example}\n\
             \t\t}}\n\
             \t}}"
        );
    }
    if let Some(example) = types.data_source_types.first() {
        // A provider can expose a resource and data source with the same
        // type name. Keep the data source import distinct in that case too.
        let _ = writeln!(
            imports,
            "\tdata_source_type {}",
            types.data_source_import(example)
        );
        let _ = writeln!(
            declarations,
            "\tdata: lookup: data_source_type.#DataSource & {{\n\
             \t\tprovider: \"{local_name}\"\n\
             \t\tconfiguration: {{\n\
             \t\t\t// arguments of {example}\n\
             \t\t}}\n\
             \t}}"
        );
    }
    format!(
        "Generated CUE types for {import} {version} ({resource_count} resource types, {data_source_count} data source types) in {directory}\n\
         and pinned it in cuenv.lock.\n\
         \n\
         Import the provider and the resource and data source types you use:\n\
         \n\
         import (\n\
         {imports}\
         )\n\
         \n\
         infrastructure: {{\n\
         \tproviders: {local_name}: {provider_alias}.#Provider\n\
         {declarations}\
         }}\n\
         \n\
         Commit cuenv.lock. {directory} is generated and listed in .gitignore:\n\
         `cuenv sync` (or `cuenv sync infrastructure`) recreates it from cuenv.lock.",
        import = types.import_path,
        resource_count = types.resource_types.len(),
        data_source_count = types.data_source_types.len(),
        directory = types.directory.display(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn added_message_supports_data_only_providers_and_duplicate_type_names() {
        let mut types = GeneratedTypes {
            import_path: "registry.terraform.io/acme/example".into(),
            package_name: "example_provider".into(),
            directory: "cue.mod/gen/registry.terraform.io/acme/example".into(),
            files: std::collections::BTreeMap::new(),
            schema_digest: format!("sha256:{}", "0".repeat(64)),
            resource_types: Vec::new(),
            data_source_types: vec!["example_lookup".into()],
        };
        let message = added_message(&types, "1.2.3");
        assert!(
            message.contains("0 resource types, 1 data source types"),
            "{message}"
        );
        assert!(
            message.contains(
                "data_source_type \"registry.terraform.io/acme/example/data/example_lookup\""
            ),
            "{message}"
        );
        assert!(
            message.contains("data: lookup: data_source_type.#DataSource"),
            "{message}"
        );
        assert!(!message.contains("#Resource"), "{message}");
        types.resource_types.push("example_lookup".into());
        let message = added_message(&types, "1.2.3");
        assert!(
            message.contains("resources: example: resource_type.#Resource"),
            "{message}"
        );
        assert!(
            message.contains("data: lookup: data_source_type.#DataSource"),
            "{message}"
        );
        assert!(message.contains("provider: \"example\""), "{message}");
    }

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
