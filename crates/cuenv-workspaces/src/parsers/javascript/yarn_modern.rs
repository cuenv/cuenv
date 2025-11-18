use crate::core::traits::LockfileParser;
use crate::core::types::{DependencyRef, DependencySource, LockfileEntry};
use crate::error::{Error, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Parser for Yarn Modern (v2+, Berry) `yarn.lock` files.
#[derive(Debug, Default, Clone, Copy)]
pub struct YarnModernLockfileParser;

impl LockfileParser for YarnModernLockfileParser {
    fn parse(&self, lockfile_path: &Path) -> Result<Vec<LockfileEntry>> {
        let contents = fs::read_to_string(lockfile_path).map_err(|source| Error::Io {
            source,
            path: Some(lockfile_path.to_path_buf()),
            operation: "reading yarn.lock".to_string(),
        })?;

        // Parse as YAML value first to extract and separate metadata
        let value: serde_yaml::Value =
            serde_yaml::from_str(&contents).map_err(|source| Error::LockfileParseFailed {
                path: lockfile_path.to_path_buf(),
                message: source.to_string(),
            })?;

        // Extract packages, excluding __metadata
        let packages = if let serde_yaml::Value::Mapping(mut map) = value {
            // Remove __metadata key if present
            map.remove(&serde_yaml::Value::String("__metadata".to_string()));

            // Deserialize remaining map into packages
            serde_yaml::from_value::<std::collections::BTreeMap<String, YarnModernPackage>>(
                serde_yaml::Value::Mapping(map),
            )
            .map_err(|source| Error::LockfileParseFailed {
                path: lockfile_path.to_path_buf(),
                message: format!("Failed to deserialize packages: {source}"),
            })?
        } else {
            return Err(Error::LockfileParseFailed {
                path: lockfile_path.to_path_buf(),
                message: "Expected YAML mapping at root level".to_string(),
            });
        };

        let mut entries = Vec::new();

        for (descriptor, package_info) in packages {
            if let Some(entry) = entry_from_package(lockfile_path, &descriptor, &package_info)? {
                entries.push(entry);
            }
        }

        Ok(entries)
    }

    fn supports_lockfile(&self, path: &Path) -> bool {
        // Check filename first as a fast pre-filter
        if !matches!(path.file_name().and_then(|n| n.to_str()), Some("yarn.lock")) {
            return false;
        }

        // If the file doesn't exist yet, we can't sniff content - accept based on filename only
        if !path.exists() {
            return true;
        }

        // Read a small prefix of the file to distinguish Yarn v2+ from v1
        // Yarn Modern (v2+) uses YAML with structures like "__metadata:"
        // Yarn Classic (v1) uses a header like "# yarn lockfile v1"
        if let Ok(contents) = fs::read_to_string(path) {
            // Yarn Modern has __metadata entries
            if contents.contains("__metadata:") {
                return true;
            }

            // If it has the v1 header, it's Classic, not Modern
            if contents.contains("# yarn lockfile v1") {
                return false;
            }

            // Check for Yarn Modern YAML-style package descriptors with @npm: protocol
            if contents.contains("@npm:") {
                // Look for quoted keys with protocol specifiers (npm:, workspace:, etc.)
                for line in contents.lines().take(30) {
                    if line.trim().starts_with('"')
                        && line.contains("@npm:")
                        && line.trim().ends_with(':')
                    {
                        // Found a Yarn Modern-style package descriptor
                        return true;
                    }
                }
            }

            // If we see unquoted v1-style descriptors, it's Classic
            for line in contents.lines().take(30) {
                if !line.starts_with(' ')
                    && !line.starts_with('\t')
                    && !line.starts_with('#')
                    && line.contains("@")
                    && line.ends_with(':')
                    && !line.starts_with('"')
                // v1 doesn't quote keys
                {
                    return false; // It's Classic, not Modern
                }
            }
        }

        // Default to false if we can't determine
        false
    }

    fn lockfile_name(&self) -> &str {
        "yarn.lock"
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct YarnModernPackage {
    /// Resolution string (e.g., "left-pad@npm:1.3.0")
    #[serde(default)]
    resolution: Option<String>,
    /// Version (sometimes present separately)
    #[serde(default)]
    version: Option<String>,
    /// Dependencies map
    #[serde(default)]
    dependencies: BTreeMap<String, String>,
    /// Dev dependencies
    #[serde(default, rename = "devDependencies")]
    dev_dependencies: BTreeMap<String, String>,
    /// Peer dependencies
    #[serde(default, rename = "peerDependencies")]
    peer_dependencies: BTreeMap<String, String>,
    /// Optional dependencies
    #[serde(default, rename = "optionalDependencies")]
    optional_dependencies: BTreeMap<String, String>,
    /// Checksum
    #[serde(default)]
    checksum: Option<String>,
    /// Language and package manager metadata
    #[serde(default, rename = "languageName")]
    language_name: Option<String>,
    /// Link type
    #[serde(default, rename = "linkType")]
    link_type: Option<String>,
}

fn entry_from_package(
    lockfile_path: &Path,
    descriptor: &str,
    package: &YarnModernPackage,
) -> Result<Option<LockfileEntry>> {
    // Parse the descriptor to get the name and version requirement
    // Descriptors look like: "left-pad@npm:^1.3.0", "@babel/core@npm:^7.0.0"
    let (name, _version_req) = parse_descriptor(lockfile_path, descriptor)?;

    // Get the resolved version from either resolution or version field
    let (version, source) = if let Some(resolution) = &package.resolution {
        parse_resolution(resolution, &name)?
    } else if let Some(version) = &package.version {
        (
            version.clone(),
            DependencySource::Registry(format!("npm:{name}@{version}")),
        )
    } else {
        return Err(Error::LockfileParseFailed {
            path: lockfile_path.to_path_buf(),
            message: format!("Package {descriptor} has no resolution or version"),
        });
    };

    // Determine if this is a workspace member
    let is_workspace_member = package
        .link_type
        .as_deref()
        .map(|lt| lt == "soft")
        .unwrap_or(false)
        || matches!(&source, DependencySource::Workspace(_));

    // Extract dependencies
    let mut dependencies = Vec::new();
    push_dependencies(&mut dependencies, &package.dependencies);
    push_dependencies(&mut dependencies, &package.dev_dependencies);
    push_dependencies(&mut dependencies, &package.peer_dependencies);
    push_dependencies(&mut dependencies, &package.optional_dependencies);

    Ok(Some(LockfileEntry {
        name,
        version,
        source,
        checksum: package.checksum.clone(),
        dependencies,
        is_workspace_member,
    }))
}

fn parse_descriptor(lockfile_path: &Path, descriptor: &str) -> Result<(String, String)> {
    // Descriptors have format: "package-name@protocol:version"
    // Examples: "left-pad@npm:^1.3.0", "@babel/core@npm:^7.0.0", "my-pkg@workspace:."

    if descriptor.starts_with('@') {
        // Scoped package: "@scope/name@protocol:version"
        if let Some(second_at) = descriptor[1..].find('@') {
            let at_idx = second_at + 1;
            let name = &descriptor[..at_idx];
            let rest = &descriptor[at_idx + 1..];
            Ok((name.to_string(), rest.to_string()))
        } else {
            Err(Error::LockfileParseFailed {
                path: lockfile_path.to_path_buf(),
                message: format!("Invalid scoped package descriptor: {descriptor}"),
            })
        }
    } else {
        // Regular package: "name@protocol:version"
        if let Some(at_idx) = descriptor.find('@') {
            let name = &descriptor[..at_idx];
            let rest = &descriptor[at_idx + 1..];
            Ok((name.to_string(), rest.to_string()))
        } else {
            Err(Error::LockfileParseFailed {
                path: lockfile_path.to_path_buf(),
                message: format!("Invalid package descriptor: {descriptor}"),
            })
        }
    }
}

fn parse_resolution(resolution: &str, package_name: &str) -> Result<(String, DependencySource)> {
    // Resolutions look like:
    // - "left-pad@npm:1.3.0"
    // - "@babel/core@npm:7.22.5"
    // - "my-package@workspace:packages/my-package"
    // - "some-lib@git+https://github.com/user/repo.git#commit:abc123"
    // - "local-dep@file:../local-dep"

    // Find the protocol separator
    if let Some(colon_idx) = resolution.find(':') {
        let before_colon = &resolution[..colon_idx];
        let after_colon = &resolution[colon_idx + 1..];

        // Extract protocol
        let protocol = if let Some(at_idx) = before_colon.rfind('@') {
            &before_colon[at_idx + 1..]
        } else {
            before_colon
        };

        match protocol {
            "npm" | "registry" => {
                // Version is after the colon
                Ok((
                    after_colon.to_string(),
                    DependencySource::Registry(format!("npm:{package_name}@{after_colon}")),
                ))
            }
            "workspace" => {
                // Workspace path is after the colon
                Ok((
                    "0.0.0".to_string(),
                    DependencySource::Workspace(PathBuf::from(after_colon)),
                ))
            }
            "git" | "git+https" | "git+ssh" => {
                // Git resolution: extract commit hash if present
                let (repo, commit) = if let Some(hash_idx) = after_colon.rfind('#') {
                    (
                        &after_colon[..hash_idx],
                        after_colon[hash_idx + 1..].to_string(),
                    )
                } else {
                    (after_colon, "HEAD".to_string())
                };
                Ok((
                    commit.clone(),
                    DependencySource::Git(format!("{protocol}:{repo}#{commit}")),
                ))
            }
            "file" => {
                // File path
                Ok((
                    "0.0.0".to_string(),
                    DependencySource::Path(PathBuf::from(after_colon)),
                ))
            }
            _ => {
                // Unknown protocol, treat as registry
                Ok((
                    after_colon.to_string(),
                    DependencySource::Registry(resolution.to_string()),
                ))
            }
        }
    } else {
        // No protocol separator, treat as version
        Ok((
            resolution.to_string(),
            DependencySource::Registry(format!("npm:{package_name}@{resolution}")),
        ))
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
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn parses_basic_yarn_modern_lock() {
        let yaml = r#"
"left-pad@npm:^1.3.0":
  version: 1.3.0
  resolution: "left-pad@npm:1.3.0"
  checksum: sha512-test123
  languageName: node
  linkType: hard

"react@npm:^18.0.0":
  version: 18.2.0
  resolution: "react@npm:18.2.0"
  dependencies:
    loose-envify: "npm:^1.1.0"
  languageName: node
  linkType: hard
"#;

        let mut file = NamedTempFile::new().unwrap();
        file.write_all(yaml.as_bytes()).unwrap();

        let parser = YarnModernLockfileParser;
        let entries = parser.parse(file.path()).unwrap();

        assert!(!entries.is_empty());

        let left_pad = entries.iter().find(|e| e.name == "left-pad");
        assert!(left_pad.is_some());
        let left_pad = left_pad.unwrap();
        assert_eq!(left_pad.version, "1.3.0");
        assert!(!left_pad.is_workspace_member);

        let react = entries.iter().find(|e| e.name == "react");
        assert!(react.is_some());
        let react = react.unwrap();
        assert_eq!(react.version, "18.2.0");
        assert_eq!(react.dependencies.len(), 1);
    }

    #[test]
    fn parses_workspace_packages() {
        let yaml = r#"
"my-package@workspace:.":
  version: 0.0.0
  resolution: "my-package@workspace:."
  linkType: soft
  languageName: unknown
"#;

        let mut file = NamedTempFile::new().unwrap();
        file.write_all(yaml.as_bytes()).unwrap();

        let parser = YarnModernLockfileParser;
        let entries = parser.parse(file.path()).unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "my-package");
        assert!(entries[0].is_workspace_member);
    }

    #[test]
    fn parses_scoped_packages() {
        let yaml = r#"
"@babel/core@npm:^7.22.0":
  version: 7.22.5
  resolution: "@babel/core@npm:7.22.5"
  languageName: node
  linkType: hard
"#;

        let mut file = NamedTempFile::new().unwrap();
        file.write_all(yaml.as_bytes()).unwrap();

        let parser = YarnModernLockfileParser;
        let entries = parser.parse(file.path()).unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "@babel/core");
        assert_eq!(entries[0].version, "7.22.5");
    }

    #[test]
    fn supports_expected_filename() {
        let parser = YarnModernLockfileParser;
        assert!(parser.supports_lockfile(Path::new("/tmp/yarn.lock")));
        assert!(!parser.supports_lockfile(Path::new("package-lock.json")));
    }

    #[test]
    fn handles_metadata_entries() {
        let yaml = r#"
__metadata:
  version: 6
  cacheKey: 8

"left-pad@npm:^1.3.0":
  version: 1.3.0
  resolution: "left-pad@npm:1.3.0"
  checksum: sha512-test123
  languageName: node
  linkType: hard
"#;

        let mut file = NamedTempFile::new().unwrap();
        file.write_all(yaml.as_bytes()).unwrap();

        let parser = YarnModernLockfileParser;
        let entries = parser.parse(file.path()).unwrap();

        // Should only have left-pad, __metadata should be excluded
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "left-pad");
        assert_eq!(entries[0].version, "1.3.0");
    }
}
