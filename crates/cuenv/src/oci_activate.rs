use cuenv::cli::CliError;
use std::path::PathBuf;

/// Run OCI binary activation (`cuenv runtime oci activate`).
///
/// Reads the lockfile, pulls/extracts binaries for the current platform,
/// and outputs PATH modifications to stdout (to be sourced by the hook system).
///
/// This command is typically invoked by the `#OCIActivate` hook defined in
/// `schema/oci.cue` to add OCI-managed binaries to the PATH.
pub async fn run_oci_activate() -> Result<(), CliError> {
    use cuenv_tools_oci::{OciCache, OciClient, current_platform};

    let lockfile_path = find_lockfile().ok_or_else(|| {
        CliError::config_with_help(
            "No cuenv.lock found",
            "Run 'cuenv sync lock' to create the lockfile",
        )
    })?;

    let lockfile = cuenv_core::lockfile::Lockfile::load(&lockfile_path)
        .map_err(|e| CliError::other(format!("Failed to load lockfile: {e}")))?
        .ok_or_else(|| {
            CliError::config_with_help(
                "Lockfile is empty",
                "Run 'cuenv sync lock' to populate the lockfile",
            )
        })?;

    let client = OciClient::new();
    let cache = OciCache::default();
    cache
        .ensure_dirs()
        .map_err(|e| CliError::other(format!("Failed to create cache directories: {e}")))?;

    let platform = current_platform();
    let bin_dirs = activate_lockfile_artifacts(&lockfile, &client, &cache, &platform).await?;

    if !bin_dirs.is_empty() {
        let mut path_additions: Vec<String> =
            bin_dirs.iter().map(|p| p.display().to_string()).collect();
        path_additions.sort();
        cuenv_events::println_redacted(&format!(
            "export PATH=\"{}:$PATH\"",
            path_additions.join(":")
        ));
    }

    Ok(())
}

/// Activate every artifact in the lockfile that matches `platform`, returning
/// the set of `bin/` directories that should be prepended to PATH.
pub async fn activate_lockfile_artifacts(
    lockfile: &cuenv_core::lockfile::Lockfile,
    client: &cuenv_tools_oci::OciClient,
    cache: &cuenv_tools_oci::OciCache,
    platform: &cuenv_tools_oci::Platform,
) -> Result<std::collections::HashSet<PathBuf>, CliError> {
    use cuenv_core::lockfile::ArtifactKind;
    use cuenv_tools_oci::extract_from_layers;
    use std::collections::HashSet;

    let platform_str = platform.to_string();
    let mut bin_dirs: HashSet<PathBuf> = HashSet::new();

    for artifact in &lockfile.artifacts {
        let Some(platform_data) = artifact.platforms.get(&platform_str) else {
            continue;
        };

        let ArtifactKind::Image { image, extract } = &artifact.kind;

        if extract.is_empty() {
            ::tracing::warn!(
                image = %image,
                "OCI image has no extract entries in the lockfile; skipping activation. \
                 Add `extract: [{{ path: ... }}]` to the image in your CUE and re-run `cuenv sync lock`."
            );
            continue;
        }

        let digest = &platform_data.digest;

        let all_cached = extract
            .iter()
            .all(|entry| cache.get_binary(digest, &entry.binary_name()).is_some());
        if all_cached {
            for entry in extract {
                if let Some(cached_path) = cache.get_binary(digest, &entry.binary_name())
                    && let Some(parent) = cached_path.parent()
                {
                    bin_dirs.insert(parent.to_path_buf());
                }
            }
            continue;
        }

        let resolved = client
            .resolve_digest(image, platform)
            .await
            .map_err(|e| CliError::other(format!("Failed to resolve '{}': {}", image, e)))?;

        let layer_paths = client.pull_layers(&resolved, cache).await.map_err(|e| {
            CliError::other(format!("Failed to pull layers for '{}': {}", image, e))
        })?;

        if layer_paths.is_empty() {
            ::tracing::warn!(
                image = %image,
                "OCI image has no layers to extract; skipping activation"
            );
            continue;
        }

        for entry in extract {
            let binary_name = entry.binary_name();
            let dest = cache.binary_path(digest, &binary_name);

            if !dest.exists() {
                extract_from_layers(&layer_paths, &entry.path, &dest).map_err(|e| {
                    CliError::other(format!(
                        "Failed to extract '{}' from '{}': {}",
                        entry.path, image, e
                    ))
                })?;
            }

            if !dest.exists() {
                return Err(CliError::other(format!(
                    "Extraction of '{}' from '{}' did not produce a file at {}",
                    entry.path,
                    image,
                    dest.display()
                )));
            }

            if let Some(parent) = dest.parent() {
                bin_dirs.insert(parent.to_path_buf());
            }
        }
    }

    Ok(bin_dirs)
}

fn find_lockfile() -> Option<PathBuf> {
    use cuenv_core::lockfile::LOCKFILE_NAME;

    let mut current = std::env::current_dir().ok()?;
    loop {
        let lockfile_path = current.join(LOCKFILE_NAME);
        if lockfile_path.exists() {
            return Some(lockfile_path);
        }

        let cue_mod_lockfile = current.join("cue.mod").join(LOCKFILE_NAME);
        if cue_mod_lockfile.exists() {
            return Some(cue_mod_lockfile);
        }

        if !current.pop() {
            return None;
        }
    }
}
