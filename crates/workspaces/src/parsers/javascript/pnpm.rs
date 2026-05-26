use crate::core::traits::LockfileParser;
use crate::core::types::{DependencyRef, DependencySource, LockfileEntry};
use crate::error::{Error, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Parser for pnpm `pnpm-lock.yaml` files.
#[derive(Debug, Default, Clone, Copy)]
pub struct PnpmLockfileParser;

impl LockfileParser for PnpmLockfileParser {
    fn parse(&self, lockfile_path: &Path) -> Result<Vec<LockfileEntry>> {
        let contents = fs::read_to_string(lockfile_path).map_err(|source| Error::Io {
            source,
            path: Some(lockfile_path.to_path_buf()),
            operation: "reading pnpm-lock.yaml".to_string(),
        })?;

        let lockfile: PnpmLockfile =
            serde_yaml::from_str(&contents).map_err(|source| Error::LockfileParseFailed {
                path: lockfile_path.to_path_buf(),
                message: source.to_string(),
            })?;

        // Validate lockfileVersion format (accept any valid numeric version)
        // We accept all numeric lockfile versions and only reject clearly invalid formats.
        // This allows the parser to work with future pnpm versions without requiring updates.
        if let Some(ref version_str) = lockfile.lockfile_version {
            // Parse version string (e.g., "5.4", "6.0", "9.0")
            let major_version = version_str
                .split('.')
                .next()
                .and_then(|v| v.trim_matches('\'').parse::<u32>().ok());

            if major_version.is_none() {
                return Err(Error::LockfileParseFailed {
                    path: lockfile_path.to_path_buf(),
                    message: format!(
                        "Invalid pnpm lockfileVersion format: '{version_str}'. Expected a numeric version like '6.0'.",
                    ),
                });
            }

            // Log a warning for versions newer than what we've tested (9.0)
            if let Some(major) = major_version
                && major > 9
            {
                tracing::warn!(
                    "Encountered pnpm lockfile version '{version_str}' which is newer than the highest tested version (9.0). Parsing may fail or be incomplete.",
                );
            }
            // Accept all valid numeric versions (no version-specific rejection)
        }
        // If lockfileVersion is missing, proceed (compatible with older pnpm versions)

        let mut entries = Vec::new();

        // Parse workspace importers (workspace members)
        for (importer_path, importer) in lockfile.importers {
            let entry = entry_from_importer(&importer_path, &importer);
            entries.push(entry);
        }

        // Parse external packages
        for (package_key, package_info) in lockfile.packages {
            let entry = entry_from_package(lockfile_path, &package_key, &package_info)?;
            entries.push(entry);
        }

        Ok(entries)
    }

    fn supports_lockfile(&self, path: &Path) -> bool {
        matches!(
            path.file_name().and_then(|n| n.to_str()),
            Some("pnpm-lock.yaml")
        )
    }

    fn lockfile_name(&self) -> &'static str {
        "pnpm-lock.yaml"
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct PnpmLockfile {
    #[serde(default)]
    lockfile_version: Option<String>,
    #[serde(default)]
    importers: BTreeMap<String, PnpmImporter>,
    #[serde(default)]
    packages: BTreeMap<String, PnpmPackage>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct PnpmImporter {
    #[serde(default)]
    dependencies: BTreeMap<String, String>,
    #[serde(default)]
    dev_dependencies: BTreeMap<String, String>,
    #[serde(default)]
    optional_dependencies: BTreeMap<String, String>,
    #[serde(default)]
    #[allow(dead_code)] // pnpm-lock.yaml format field for deserialization
    specifiers: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct PnpmPackage {
    #[serde(default)]
    resolution: Option<PnpmResolution>,
    #[serde(default)]
    dependencies: BTreeMap<String, String>,
    #[serde(default)]
    dev_dependencies: BTreeMap<String, String>,
    #[serde(default)]
    optional_dependencies: BTreeMap<String, String>,
    #[serde(default)]
    peer_dependencies: BTreeMap<String, String>,
    /// Integrity checksum (e.g., "sha512-...")
    #[serde(default)]
    integrity: Option<String>,
    #[serde(default)]
    #[allow(dead_code)] // pnpm-lock.yaml format field for deserialization
    dev: bool,
    #[serde(default)]
    #[allow(dead_code)] // pnpm-lock.yaml format field for deserialization
    optional: bool,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum PnpmResolution {
    Registry { integrity: String, tarball: String },
    Git { repo: String, commit: String },
    Object(BTreeMap<String, serde_yaml::Value>),
}

fn entry_from_importer(importer_path: &str, importer: &PnpmImporter) -> LockfileEntry {
    let name = if importer_path == "." {
        "workspace-root".to_string()
    } else {
        importer_path
            .trim_start_matches("./")
            .rsplit('/')
            .next()
            .unwrap_or(importer_path)
            .to_string()
    };

    let mut dependencies = Vec::new();
    push_dependencies(&mut dependencies, &importer.dependencies);
    push_dependencies(&mut dependencies, &importer.dev_dependencies);
    push_dependencies(&mut dependencies, &importer.optional_dependencies);

    let path = if importer_path == "." {
        PathBuf::from(".")
    } else {
        PathBuf::from(importer_path.trim_start_matches("./"))
    };

    LockfileEntry {
        name,
        version: "0.0.0".to_string(),
        source: DependencySource::Workspace(path),
        checksum: None,
        dependencies,
        is_workspace_member: true,
    }
}

fn entry_from_package(
    lockfile_path: &Path,
    package_key: &str,
    package_info: &PnpmPackage,
) -> Result<LockfileEntry> {
    // pnpm package keys look like: "/@babel/core/7.22.5" or "/left-pad/1.3.0"
    let (name, version) = parse_package_key(lockfile_path, package_key)?;

    let source = determine_source(&name, package_info);

    // Extract checksum from either the top-level integrity field or from the resolution
    let checksum = package_info.integrity.clone().or_else(|| {
        package_info.resolution.as_ref().and_then(|res| match res {
            PnpmResolution::Registry { integrity, .. } => Some(integrity.clone()),
            PnpmResolution::Object(map) => map
                .get("integrity")
                .and_then(|v| v.as_str())
                .map(ToString::to_string),
            PnpmResolution::Git { .. } => None,
        })
    });

    let mut dependencies = Vec::new();
    push_dependencies(&mut dependencies, &package_info.dependencies);
    push_dependencies(&mut dependencies, &package_info.dev_dependencies);
    push_dependencies(&mut dependencies, &package_info.optional_dependencies);
    push_dependencies(&mut dependencies, &package_info.peer_dependencies);

    Ok(LockfileEntry {
        name,
        version,
        source,
        checksum,
        dependencies,
        is_workspace_member: false,
    })
}

fn parse_package_key(lockfile_path: &Path, package_key: &str) -> Result<(String, String)> {
    let key = package_key.trim_start_matches('/');

    parse_scoped_package_key(key)
        .or_else(|| parse_regular_package_key(key))
        .ok_or_else(|| Error::LockfileParseFailed {
            path: lockfile_path.to_path_buf(),
            message: format!("Invalid pnpm package key format: {package_key}"),
        })
}

fn parse_scoped_package_key(key: &str) -> Option<(String, String)> {
    if !key.starts_with('@') {
        return None;
    }

    let mut parts = key.splitn(3, '/');
    let scope = parts.next()?;
    let package = parts.next()?;

    if let Some(version) = parts.next() {
        Some((
            format!("{scope}/{package}"),
            strip_peer_suffix(version).to_string(),
        ))
    } else {
        let (package, version) = package.rsplit_once('@')?;
        Some((
            format!("{scope}/{package}"),
            strip_peer_suffix(version).to_string(),
        ))
    }
}

fn parse_regular_package_key(key: &str) -> Option<(String, String)> {
    key.rsplit_once('@')
        .or_else(|| key.rsplit_once('/'))
        .map(|(name, version)| (name.to_string(), strip_peer_suffix(version).to_string()))
}

fn strip_peer_suffix(version: &str) -> &str {
    version
        .split('(')
        .next()
        .unwrap_or(version)
        .trim_end_matches(')')
}

#[allow(clippy::option_if_let_else)] // Complex parsing with nested conditionals - imperative is clearer
fn determine_source(name: &str, package_info: &PnpmPackage) -> DependencySource {
    if let Some(resolution) = &package_info.resolution {
        match resolution {
            PnpmResolution::Registry { tarball, .. } => DependencySource::Registry(tarball.clone()),
            PnpmResolution::Git { repo, commit } => {
                DependencySource::Git(format!("{repo}#{commit}"))
            }
            PnpmResolution::Object(map) => {
                // Check for various resolution types
                if let Some(tarball) = map.get("tarball").and_then(|v| v.as_str()) {
                    DependencySource::Registry(tarball.to_string())
                } else if let Some(repo) = map.get("repo").and_then(|v| v.as_str()) {
                    let commit = map.get("commit").and_then(|v| v.as_str()).unwrap_or("HEAD");
                    DependencySource::Git(format!("{repo}#{commit}"))
                } else if let Some(dir) = map.get("directory").and_then(|v| v.as_str()) {
                    DependencySource::Path(PathBuf::from(dir))
                } else {
                    // Default to registry with package name
                    DependencySource::Registry(format!("npm:{name}"))
                }
            }
        }
    } else {
        // No resolution info, assume npm registry
        DependencySource::Registry(format!("npm:{name}"))
    }
}

fn push_dependencies(target: &mut Vec<DependencyRef>, deps: &BTreeMap<String, String>) {
    for (name, version_req) in deps {
        target.push(DependencyRef {
            name: name.clone(),
            version_req: version_req.clone(),
        });
    }
}

#[cfg(test)]
#[allow(clippy::needless_raw_string_hashes, clippy::uninlined_format_args)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn parses_basic_pnpm_lock() {
        let yaml = r#"
lockfileVersion: '6.0'

importers:
  .:
    dependencies:
      left-pad: 1.3.0

packages:
  /left-pad@1.3.0:
    resolution:
      integrity: sha512-test123
      tarball: https://registry.npmjs.org/left-pad/-/left-pad-1.3.0.tgz
    dev: false
"#;

        let mut file = NamedTempFile::new().unwrap();
        file.write_all(yaml.as_bytes()).unwrap();

        let parser = PnpmLockfileParser;
        let entries = parser.parse(file.path()).unwrap();

        assert!(entries.len() >= 2);

        let workspace = entries
            .iter()
            .find(|e| e.is_workspace_member)
            .expect("workspace root");
        assert_eq!(workspace.dependencies.len(), 1);

        let left_pad = entries
            .iter()
            .find(|e| e.name == "left-pad")
            .expect("left-pad");
        assert_eq!(left_pad.version, "1.3.0");
        assert!(!left_pad.is_workspace_member);
    }

    #[test]
    fn parses_scoped_packages() {
        let yaml = r#"
lockfileVersion: '6.0'

importers:
  .:
    dependencies: {}

packages:
  /@babel/core@7.22.5:
    resolution:
      integrity: sha512-xyz
      tarball: https://registry.npmjs.org/@babel/core/-/core-7.22.5.tgz
    dev: false
"#;

        let mut file = NamedTempFile::new().unwrap();
        file.write_all(yaml.as_bytes()).unwrap();

        let parser = PnpmLockfileParser;
        let entries = parser.parse(file.path()).unwrap();

        let babel = entries
            .iter()
            .find(|e| e.name == "@babel/core")
            .expect("@babel/core");
        assert_eq!(babel.version, "7.22.5");
    }

    #[test]
    fn supports_expected_filename() {
        let parser = PnpmLockfileParser;
        assert!(parser.supports_lockfile(Path::new("/tmp/pnpm-lock.yaml")));
        assert!(!parser.supports_lockfile(Path::new("package-lock.json")));
    }

    #[test]
    fn accepts_various_lockfile_versions() {
        // Test that we accept various lockfile versions (5.4, 6.0, 9.0, etc.)
        for version in ["5.4", "6.0", "7.0", "9.0", "10.0"] {
            let yaml = format!(
                r#"
lockfileVersion: '{}'

importers:
  .:
    dependencies:
      left-pad: 1.3.0

packages:
  /left-pad@1.3.0:
    resolution:
      integrity: sha512-test
      tarball: https://registry.npmjs.org/left-pad/-/left-pad-1.3.0.tgz
    dev: false
"#,
                version
            );

            let mut file = NamedTempFile::new().unwrap();
            file.write_all(yaml.as_bytes()).unwrap();

            let parser = PnpmLockfileParser;
            let result = parser.parse(file.path());
            assert!(
                result.is_ok(),
                "Version {} should be accepted, got error: {:?}",
                version,
                result.err()
            );
        }
    }

    #[test]
    fn rejects_invalid_lockfile_version_format() {
        let yaml = r#"
lockfileVersion: 'invalid'

importers:
  .:
    dependencies: {}

packages: {}
"#;

        let mut file = NamedTempFile::new().unwrap();
        file.write_all(yaml.as_bytes()).unwrap();

        let parser = PnpmLockfileParser;
        let err = parser.parse(file.path()).unwrap_err();

        match err {
            Error::LockfileParseFailed { message, .. } => {
                assert!(message.contains("Invalid pnpm lockfileVersion format"));
                assert!(message.contains("invalid"));
            }
            other => panic!("Expected LockfileParseFailed, got: {:?}", other),
        }
    }

    #[test]
    fn accepts_supported_versions() {
        for version in ["6.0", "9.0"] {
            let yaml = format!(
                r#"
lockfileVersion: '{}'

importers:
  .:
    dependencies:
      left-pad: 1.3.0

packages:
  /left-pad@1.3.0:
    resolution:
      integrity: sha512-test
      tarball: https://registry.npmjs.org/left-pad/-/left-pad-1.3.0.tgz
    dev: false
"#,
                version
            );

            let mut file = NamedTempFile::new().unwrap();
            file.write_all(yaml.as_bytes()).unwrap();

            let parser = PnpmLockfileParser;
            let result = parser.parse(file.path());
            assert!(
                result.is_ok(),
                "Version {} should be supported, got error: {:?}",
                version,
                result.err()
            );
        }
    }

    #[test]
    fn accepts_missing_lockfile_version() {
        // Older pnpm versions may not have lockfileVersion
        let yaml = r#"
importers:
  .:
    dependencies:
      left-pad: 1.3.0

packages:
  /left-pad@1.3.0:
    resolution:
      integrity: sha512-test
      tarball: https://registry.npmjs.org/left-pad/-/left-pad-1.3.0.tgz
    dev: false
"#;

        let mut file = NamedTempFile::new().unwrap();
        file.write_all(yaml.as_bytes()).unwrap();

        let parser = PnpmLockfileParser;
        let result = parser.parse(file.path());
        assert!(
            result.is_ok(),
            "Missing lockfileVersion should be accepted, got error: {:?}",
            result.err()
        );
    }
}
