//! Generated CUE types for infrastructure providers: shared by
//! `cuenv infrastructure provider add|remove` and `cuenv sync infrastructure`.
//!
//! Types are rendered from the schema of the exact provider release
//! `cuenv.lock` pins (see [`cuenv_infrastructure::cue_types`]) into
//! `cue.mod/gen/<hostname>/<namespace>/<type>` of the project's CUE module.
//! A provider directory is only ever replaced or removed when its
//! `provider.cue` carries cuenv's generated-code header.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use cuenv_core::lockfile::{LOCKFILE_NAME, LockedInfrastructureProvider, Lockfile};
use cuenv_infrastructure::Cancellation;
use cuenv_infrastructure::cue_types::{
    self, GENERATED_HEADER, GeneratedTypes, PROVIDER_FILE_NAME, ProviderRelease,
};
use cuenv_infrastructure::registry::{
    ProviderInstaller, ProviderSource, current_platform, default_cache_directory,
};

use crate::cli::CliError;
use crate::commands::env_file::find_cue_module_root;

/// The CUE module the types and the lock entry belong to.
#[derive(Debug, Clone)]
pub struct Module {
    /// Directory holding `cue.mod`.
    pub root: PathBuf,
}

impl Module {
    /// The CUE module containing `path` (the nearest `cue.mod` above it).
    pub fn containing(path: &Path) -> Result<Self, CliError> {
        find_cue_module_root(path)
            .map(|root| Self { root })
            .ok_or_else(|| {
                CliError::config_with_help(
                    format!("{} is not inside a CUE module", path.display()),
                    "Generated provider types live in the CUE module's cue.mod/gen; run \
                     `cue mod init` first.",
                )
            })
    }

    pub fn lockfile_path(&self) -> PathBuf {
        self.root.join(LOCKFILE_NAME)
    }

    /// The module's lockfile, or a new one.
    pub fn lockfile(&self) -> Result<Lockfile, CliError> {
        Lockfile::load(&self.lockfile_path())
            .map(Option::unwrap_or_default)
            .map_err(|error| CliError::config(error.to_string()))
    }
}

/// Install a pinned provider release and render its types.
pub struct Generation<'generation> {
    pub source: &'generation ProviderSource,
    pub version: &'generation str,
    /// The pin's archive SHA-256 for every platform (hex, no prefix).
    pub platforms: &'generation BTreeMap<String, String>,
}

/// Rendered types, with warnings the provider reported.
pub struct Generated {
    pub types: GeneratedTypes,
    pub warnings: Vec<String>,
}

impl Generation<'_> {
    /// Install the release (requiring the pinned archive for this platform),
    /// read its schema and render the types.
    pub async fn run(&self) -> Result<Generated, CliError> {
        let platform = current_platform().map_err(|error| infrastructure_error(&error))?;
        let pin = self.platforms.get(&platform.to_string()).ok_or_else(|| {
            CliError::config(format!(
                "{} {} is not published for {platform}",
                self.source, self.version
            ))
        })?;
        let installer = ProviderInstaller::new(default_cache_directory())
            .map_err(|error| infrastructure_error(&error))?;
        let binary = installer
            .ensure_pinned(self.source, self.version, pin)
            .await
            .map_err(|error| infrastructure_error(&error))?;
        let (schema, warnings) = cue_types::load_schema(&binary, &Cancellation::default())
            .await
            .map_err(|error| infrastructure_error(&error))?;
        let types = cue_types::render(
            &schema,
            ProviderRelease {
                source: self.source,
                version: self.version,
            },
        )
        .map_err(|error| infrastructure_error(&error))?;
        Ok(Generated { types, warnings })
    }
}

/// The pinned archives of `pin`, without their `sha256:` prefix.
pub fn pinned_archives(pin: &LockedInfrastructureProvider) -> BTreeMap<String, String> {
    pin.platforms
        .iter()
        .filter_map(|(platform, digest)| {
            digest
                .strip_prefix("sha256:")
                .map(|hex| (platform.clone(), hex.to_string()))
        })
        .collect()
}

pub fn infrastructure_error(error: &cuenv_infrastructure::InfrastructureError) -> CliError {
    CliError::config(
        cuenv_infrastructure::strip_control_characters_except_newlines(&cuenv_events::redact(
            &error.to_string(),
        )),
    )
}

/// How generated files on disk differ from freshly rendered ones.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Differences {
    /// Files to create or rewrite, relative to the module root.
    pub changed: Vec<PathBuf>,
    /// Files to delete, relative to the module root.
    pub removed: Vec<PathBuf>,
}

impl Differences {
    pub fn is_empty(&self) -> bool {
        self.changed.is_empty() && self.removed.is_empty()
    }
}

/// Compare the provider directory on disk with `types`.
///
/// # Errors
///
/// Refuses a directory cuenv did not generate.
pub fn differences(module: &Module, types: &GeneratedTypes) -> Result<Differences, CliError> {
    let directory = module.root.join(&types.directory);
    let existing = owned_files(&directory)?;
    let mut differences = Differences::default();
    for (relative, content) in &types.files {
        let current = existing.get(relative);
        if current.is_none_or(|current| current != content) {
            differences.changed.push(types.directory.join(relative));
        }
    }
    for relative in existing.keys() {
        if !types.files.contains_key(relative) {
            differences.removed.push(types.directory.join(relative));
        }
    }
    Ok(differences)
}

/// Replace the provider directory with `types`.
pub fn write(module: &Module, types: &GeneratedTypes) -> Result<(), CliError> {
    let directory = module.root.join(&types.directory);
    owned_files(&directory)?;
    remove_owned_directory(&directory)?;
    for (relative, content) in &types.files {
        let path = directory.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| io_error("create", parent, &error))?;
        }
        std::fs::write(&path, content).map_err(|error| io_error("write", &path, &error))?;
    }
    Ok(())
}

/// Remove a provider directory cuenv generated. Returns whether there was
/// one.
pub fn remove(module: &Module, source: &ProviderSource) -> Result<bool, CliError> {
    let relative = cue_types::directory(source).map_err(|error| infrastructure_error(&error))?;
    let directory = module.root.join(relative);
    if std::fs::symlink_metadata(&directory).is_err() {
        return Ok(false);
    }
    owned_files(&directory)?;
    remove_owned_directory(&directory)?;
    remove_empty_parents(module, &directory);
    Ok(true)
}

/// Remove directories left empty above a removed provider directory, up to
/// and including `cue.mod/gen`.
fn remove_empty_parents(module: &Module, directory: &Path) {
    let generated = module.root.join("cue.mod");
    let mut current = directory.parent();
    while let Some(parent) = current {
        if parent == generated || !parent.starts_with(&generated) {
            break;
        }
        // Fails, and stops, at the first directory that is not empty.
        if std::fs::remove_dir(parent).is_err() {
            break;
        }
        current = parent.parent();
    }
}

fn remove_owned_directory(directory: &Path) -> Result<(), CliError> {
    match std::fs::remove_dir_all(directory) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io_error("remove", directory, &error)),
    }
}

/// Every file under a provider directory cuenv generated, keyed by path
/// relative to it; empty when the directory does not exist.
///
/// # Errors
///
/// Refuses a directory whose `provider.cue` lacks the generated-code header,
/// and symbolic links anywhere inside it.
fn owned_files(directory: &Path) -> Result<BTreeMap<PathBuf, String>, CliError> {
    let mut files = BTreeMap::new();
    match std::fs::symlink_metadata(directory) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(files),
        Err(error) => return Err(io_error("inspect", directory, &error)),
        Ok(metadata) if !metadata.is_dir() => return Err(not_generated(directory)),
        Ok(_) => {}
    }
    let provider = std::fs::read_to_string(directory.join(PROVIDER_FILE_NAME))
        .map_err(|_| not_generated(directory))?;
    if !provider.starts_with(GENERATED_HEADER) {
        return Err(not_generated(directory));
    }
    let mut pending = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        let current = directory.join(&relative);
        let entries =
            std::fs::read_dir(&current).map_err(|error| io_error("read", &current, &error))?;
        for entry in entries {
            let entry = entry.map_err(|error| io_error("read", &current, &error))?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|error| io_error("inspect", &path, &error))?;
            let name = relative.join(entry.file_name());
            if file_type.is_dir() {
                pending.push(name);
            } else if file_type.is_file() {
                let content = std::fs::read_to_string(&path)
                    .map_err(|error| io_error("read", &path, &error))?;
                files.insert(name, content);
            } else {
                return Err(CliError::config(format!(
                    "{} is not a regular file; generated provider types never contain one, so \
                     cuenv leaves the directory alone",
                    path.display()
                )));
            }
        }
    }
    Ok(files)
}

fn not_generated(directory: &Path) -> CliError {
    CliError::config_with_help(
        format!(
            "{} was not generated by cuenv (its {PROVIDER_FILE_NAME} does not start with \
             \"{GENERATED_HEADER}\")",
            directory.display()
        ),
        "Move it aside; cuenv only replaces or removes provider types it generated.",
    )
}

fn io_error(action: &str, path: &Path, error: &std::io::Error) -> CliError {
    CliError::config(format!("cannot {action} {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn types(files: &[(&str, &str)]) -> GeneratedTypes {
        GeneratedTypes {
            import_path: "registry.terraform.io/acme/example".to_string(),
            package_name: "example".to_string(),
            directory: PathBuf::from("cue.mod/gen/registry.terraform.io/acme/example"),
            files: files
                .iter()
                .map(|(path, content)| (PathBuf::from(path), (*content).to_string()))
                .collect(),
            schema_digest: format!("sha256:{}", "0".repeat(64)),
            resource_types: Vec::new(),
        }
    }

    fn generated(body: &str) -> String {
        format!("{GENERATED_HEADER}\n{body}")
    }

    #[test]
    fn writing_replaces_the_directory_and_then_matches() {
        let root = tempfile::tempdir().unwrap();
        let module = Module {
            root: root.path().to_path_buf(),
        };
        let first = types(&[
            ("provider.cue", &generated("a")),
            ("resources/example_old/resource.cue", "old"),
        ]);
        assert_eq!(differences(&module, &first).unwrap().changed.len(), 2);
        write(&module, &first).unwrap();
        assert!(differences(&module, &first).unwrap().is_empty());

        let second = types(&[
            ("provider.cue", &generated("b")),
            ("resources/example_new/resource.cue", "new"),
        ]);
        let found = differences(&module, &second).unwrap();
        assert_eq!(found.changed.len(), 2, "{found:?}");
        assert_eq!(
            found.removed,
            [first.directory.join("resources/example_old/resource.cue")]
        );
        write(&module, &second).unwrap();
        assert!(differences(&module, &second).unwrap().is_empty());
        assert!(
            !root
                .path()
                .join(&first.directory)
                .join("resources/example_old")
                .exists()
        );
    }

    #[test]
    fn a_directory_cuenv_did_not_generate_is_left_alone() {
        let root = tempfile::tempdir().unwrap();
        let module = Module {
            root: root.path().to_path_buf(),
        };
        let wanted = types(&[("provider.cue", &generated("a"))]);
        let directory = root.path().join(&wanted.directory);
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("provider.cue"), "package mine\n").unwrap();
        for result in [
            differences(&module, &wanted).map(|_| ()),
            write(&module, &wanted),
            remove(&module, &ProviderSource::parse("acme/example").unwrap()).map(|_| ()),
        ] {
            let error = result.unwrap_err();
            assert!(
                error.message().contains("was not generated by cuenv"),
                "{error:?}"
            );
        }
        assert_eq!(
            std::fs::read_to_string(directory.join("provider.cue")).unwrap(),
            "package mine\n"
        );
    }

    #[test]
    fn removing_deletes_only_generated_directories() {
        let root = tempfile::tempdir().unwrap();
        let module = Module {
            root: root.path().to_path_buf(),
        };
        let source = ProviderSource::parse("acme/example").unwrap();
        assert!(!remove(&module, &source).unwrap());
        write(&module, &types(&[("provider.cue", &generated("a"))])).unwrap();
        assert!(remove(&module, &source).unwrap());
        assert!(!root.path().join("cue.mod/gen").exists());
        assert!(root.path().join("cue.mod").is_dir());
    }
}
