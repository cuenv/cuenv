use crate::core::traits::LockfileParser;
use crate::core::types::{DependencyRef, DependencySource, LockfileEntry};
use crate::error::{Error, Result};
use serde::Deserialize;
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Parser for Bun `bun.lock` files (text/JSONC). Binary `bun.lockb` is rejected.
#[derive(Debug, Default, Clone, Copy)]
pub struct BunLockfileParser;

impl LockfileParser for BunLockfileParser {
    fn parse(&self, lockfile_path: &Path) -> Result<Vec<LockfileEntry>> {
        // Check if this is the binary bun.lockb format
        if lockfile_path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n == "bun.lockb")
        {
            return Err(Error::LockfileParseFailed {
                path: lockfile_path.to_path_buf(),
                message: "Binary Bun lockfile format (bun.lockb) is currently unsupported. Only the text-based JSONC format (bun.lock) is supported.".to_string(),
            });
        }

        let contents = fs::read_to_string(lockfile_path).map_err(|source| Error::Io {
            source,
            path: Some(lockfile_path.to_path_buf()),
            operation: "reading bun.lock".to_string(),
        })?;

        // Parse JSONC using jsonc-parser and convert to serde_json::Value
        let json_value =
            jsonc_parser::parse_to_value(&contents, &jsonc_parser::ParseOptions::default())
                .map_err(|err| Error::LockfileParseFailed {
                    path: lockfile_path.to_path_buf(),
                    message: format!("Failed to parse bun.lock as JSONC: {err:?}"),
                })?
                .ok_or_else(|| Error::LockfileParseFailed {
                    path: lockfile_path.to_path_buf(),
                    message: "Empty or invalid JSONC content".to_string(),
                })?;

        // Convert jsonc_parser::JsonValue to serde_json::Value
        let value: Value = convert_jsonc_to_serde_value(json_value);

        let lockfile: BunLockfile =
            serde_json::from_value(value).map_err(|err| Error::LockfileParseFailed {
                path: lockfile_path.to_path_buf(),
                message: format!("Failed to deserialize bun.lock: {err}"),
            })?;

        let version = lockfile.lockfile_version.unwrap_or(0);
        if version > 1 {
            return Err(Error::LockfileParseFailed {
                path: lockfile_path.to_path_buf(),
                message: format!(
                    "Unsupported bun lockfileVersion {version} – supported versions are 0 and 1"
                ),
            });
        }

        let mut entries = Vec::new();

        for (workspace_path, workspace) in &lockfile.workspaces {
            entries.push(entry_from_workspace(workspace_path, workspace));
        }

        for (package_name, raw_value) in lockfile.packages {
            let entry =
                entry_from_package(lockfile_path, &package_name, raw_value).map_err(|message| {
                    Error::LockfileParseFailed {
                        path: lockfile_path.to_path_buf(),
                        message: format!("{package_name}: {message}"),
                    }
                })?;
            entries.push(entry);
        }

        Ok(entries)
    }

    fn supports_lockfile(&self, path: &Path) -> bool {
        // Only support the text-based JSONC format, not the binary bun.lockb
        matches!(path.file_name().and_then(|n| n.to_str()), Some("bun.lock"))
    }

    fn lockfile_name(&self) -> &'static str {
        "bun.lock"
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct BunLockfile {
    #[serde(default)]
    lockfile_version: Option<u32>,
    #[serde(default)]
    workspaces: BTreeMap<String, BunWorkspace>,
    #[serde(default)]
    packages: BTreeMap<String, Value>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct BunWorkspace {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    dependencies: BTreeMap<String, String>,
    #[serde(default, rename = "devDependencies")]
    dev_dependencies: BTreeMap<String, String>,
    #[serde(default, rename = "optionalDependencies")]
    optional_dependencies: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct BunPackageMetadata {
    #[serde(default)]
    dependencies: BTreeMap<String, String>,
    #[serde(default, rename = "devDependencies")]
    dev_dependencies: BTreeMap<String, String>,
    #[serde(default, rename = "optionalDependencies")]
    optional_dependencies: BTreeMap<String, String>,
    #[serde(default, rename = "peerDependencies")]
    peer_dependencies: BTreeMap<String, String>,
    #[serde(default)]
    integrity: Option<String>,
    #[serde(default)]
    checksum: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BunPackageObject {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    resolution: Option<String>,
    #[serde(default)]
    locator: Option<String>,
    #[serde(default)]
    checksum: Option<String>,
    #[serde(default)]
    integrity: Option<String>,
    #[serde(default)]
    dependencies: BTreeMap<String, String>,
    #[serde(default, rename = "devDependencies")]
    dev_dependencies: BTreeMap<String, String>,
    #[serde(default, rename = "optionalDependencies")]
    optional_dependencies: BTreeMap<String, String>,
    #[serde(default, rename = "peerDependencies")]
    peer_dependencies: BTreeMap<String, String>,
}

fn entry_from_workspace(path_key: &str, workspace: &BunWorkspace) -> LockfileEntry {
    let name = workspace
        .name
        .as_deref()
        .filter(|value| !value.is_empty())
        .map_or_else(|| workspace_name_from_path(path_key), ToString::to_string);

    let mut dependencies = Vec::new();
    push_dependencies(&mut dependencies, &workspace.dependencies);
    push_dependencies(&mut dependencies, &workspace.dev_dependencies);
    push_dependencies(&mut dependencies, &workspace.optional_dependencies);

    LockfileEntry {
        name,
        version: workspace
            .version
            .clone()
            .unwrap_or_else(|| "0.0.0".to_string()),
        source: DependencySource::Workspace(workspace_path(path_key)),
        checksum: None,
        dependencies,
        is_workspace_member: true,
    }
}

fn workspace_name_from_path(path_key: &str) -> String {
    if path_key.is_empty() {
        "workspace".to_string()
    } else {
        path_key.to_string()
    }
}

fn entry_from_package(lockfile_path: &Path, name: &str, raw_value: Value) -> Result<LockfileEntry> {
    match raw_value {
        Value::Array(items) => parse_array_package(lockfile_path, name, &items),
        Value::Object(map) => parse_object_package(lockfile_path, name, map),
        other => Err(Error::LockfileParseFailed {
            path: lockfile_path.to_path_buf(),
            message: format!(
                "Unsupported package entry for {name}. Expected array or object, found {other}"
            ),
        }),
    }
}

fn parse_array_package(lockfile_path: &Path, name: &str, items: &[Value]) -> Result<LockfileEntry> {
    if items.is_empty() {
        return Err(Error::LockfileParseFailed {
            path: lockfile_path.to_path_buf(),
            message: format!("Package tuple for {name} is empty"),
        });
    }

    let locator =
        items
            .first()
            .and_then(Value::as_str)
            .ok_or_else(|| Error::LockfileParseFailed {
                path: lockfile_path.to_path_buf(),
                message: format!("Package {name} missing locator entry"),
            })?;

    let mut checksum_override = None;
    let metadata_val = items
        .get(2)
        .cloned()
        .unwrap_or(Value::Object(Map::default()));

    let metadata: BunPackageMetadata = match metadata_val {
        Value::Object(_) => serde_json::from_value(metadata_val).map_err(|err| {
            Error::LockfileParseFailed {
                path: lockfile_path.to_path_buf(),
                message: format!("{name}: invalid metadata object: {err}"),
            }
        })?,
        Value::String(s) => {
            // Some lockfiles use a terse tuple form where the third slot is a
            // checksum string instead of an object. Treat it as a checksum and
            // otherwise fall back to default metadata.
            checksum_override = Some(s);
            BunPackageMetadata::default()
        }
        _ => BunPackageMetadata::default(),
    };

    let checksum = items
        .get(3)
        .and_then(Value::as_str)
        .map(ToString::to_string)
        .or_else(|| metadata.integrity.clone())
        .or_else(|| metadata.checksum.clone())
        .or(checksum_override);

    build_package_entry(lockfile_path, name, locator, checksum, &metadata)
}

fn parse_object_package(
    lockfile_path: &Path,
    name: &str,
    map: Map<String, Value>,
) -> Result<LockfileEntry> {
    let package: BunPackageObject =
        serde_json::from_value(Value::Object(map)).map_err(|err| Error::LockfileParseFailed {
            path: lockfile_path.to_path_buf(),
            message: format!("{name}: failed to parse object entry: {err}"),
        })?;

    let BunPackageObject {
        name: _object_name,
        version,
        resolution,
        locator,
        checksum: raw_checksum,
        integrity,
        dependencies,
        dev_dependencies,
        optional_dependencies,
        peer_dependencies,
    } = package;

    let locator = locator
        .or(resolution)
        .unwrap_or_else(|| format!("{name}@{}", version.unwrap_or_default()));

    let checksum = raw_checksum.clone().or_else(|| integrity.clone());

    let metadata = BunPackageMetadata {
        dependencies,
        dev_dependencies,
        optional_dependencies,
        peer_dependencies,
        integrity,
        checksum: raw_checksum,
    };

    build_package_entry(lockfile_path, name, &locator, checksum, &metadata)
}

fn build_package_entry(
    lockfile_path: &Path,
    package_name: &str,
    locator: &str,
    checksum: Option<String>,
    metadata: &BunPackageMetadata,
) -> Result<LockfileEntry> {
    let locator_info = parse_locator(lockfile_path, locator, package_name)?;

    let mut dependencies = Vec::new();
    push_dependencies(&mut dependencies, &metadata.dependencies);
    push_dependencies(&mut dependencies, &metadata.dev_dependencies);
    push_dependencies(&mut dependencies, &metadata.optional_dependencies);
    push_dependencies(&mut dependencies, &metadata.peer_dependencies);

    Ok(LockfileEntry {
        name: package_name.to_string(),
        version: locator_info.version,
        source: locator_info.source,
        checksum,
        dependencies,
        is_workspace_member: false,
    })
}

fn push_dependencies(target: &mut Vec<DependencyRef>, deps: &BTreeMap<String, String>) {
    for (name, version_req) in deps {
        target.push(DependencyRef {
            name: name.clone(),
            version_req: version_req.clone(),
        });
    }
}

struct LocatorDetails {
    version: String,
    source: DependencySource,
}

fn parse_locator(
    lockfile_path: &Path,
    locator: &str,
    package_name: &str,
) -> Result<LocatorDetails> {
    let trimmed = locator.trim();
    if trimmed.is_empty() {
        return Err(Error::LockfileParseFailed {
            path: lockfile_path.to_path_buf(),
            message: format!("Package {package_name} has empty locator"),
        });
    }

    if let Some(rest) = trimmed.strip_prefix("workspace:") {
        return Ok(LocatorDetails {
            version: "0.0.0".to_string(),
            source: DependencySource::Workspace(workspace_path(rest)),
        });
    }

    let spec_part = match trimmed.rfind('@') {
        Some(idx) if idx + 1 < trimmed.len() => &trimmed[idx + 1..],
        _ => trimmed,
    };

    let (protocol, remainder) = if let Some(colon_idx) = spec_part.find(':') {
        (&spec_part[..colon_idx], &spec_part[colon_idx + 1..])
    } else {
        ("npm", spec_part)
    };

    let remainder = remainder.trim();
    let source = match protocol {
        "npm" | "registry" => DependencySource::Registry(format!("npm:{package_name}@{remainder}")),
        "github" => DependencySource::Git(format!("https://github.com/{remainder}")),
        "git" | "git+https" | "git+ssh" => DependencySource::Git(format!("{protocol}:{remainder}")),
        "file" => DependencySource::Path(PathBuf::from(remainder)),
        "workspace" => DependencySource::Workspace(workspace_path(remainder)),
        other => DependencySource::Registry(format!("{other}:{remainder}")),
    };

    let version = match protocol {
        "github" => remainder.split('#').nth(1).unwrap_or(remainder).to_string(),
        "file" | "workspace" => "0.0.0".to_string(),
        _ => remainder.to_string(),
    };

    Ok(LocatorDetails { version, source })
}

fn workspace_path(path: &str) -> PathBuf {
    if path.is_empty() {
        PathBuf::from(".")
    } else {
        PathBuf::from(path)
    }
}

// Convert jsonc_parser::JsonValue to serde_json::Value
fn convert_jsonc_to_serde_value(jsonc_value: jsonc_parser::JsonValue) -> Value {
    match jsonc_value {
        jsonc_parser::JsonValue::Null => Value::Null,
        jsonc_parser::JsonValue::Boolean(b) => Value::Bool(b),
        jsonc_parser::JsonValue::Number(n) => {
            // Try to parse as i64 first, then f64
            if let Ok(i) = n.parse::<i64>() {
                Value::Number(i.into())
            } else if let Ok(f) = n.parse::<f64>() {
                serde_json::Number::from_f64(f).map_or(Value::Null, Value::Number)
            } else {
                Value::Null
            }
        }
        jsonc_parser::JsonValue::String(s) => Value::String(s.to_string()),
        jsonc_parser::JsonValue::Array(arr) => {
            Value::Array(arr.into_iter().map(convert_jsonc_to_serde_value).collect())
        }
        jsonc_parser::JsonValue::Object(obj) => {
            let mut map = serde_json::Map::new();
            // Iterate over owned entries without cloning
            for (key, value) in obj {
                map.insert(key, convert_jsonc_to_serde_value(value));
            }
            Value::Object(map)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::Path;
    use tempfile::NamedTempFile;

    fn write_lock(contents: &str) -> NamedTempFile {
        let mut file = NamedTempFile::new().expect("temp file");
        file.write_all(contents.as_bytes()).expect("write lockfile");
        file
    }

    #[test]
    fn parses_basic_bun_lock() {
        let lock = r#"
        // Bun lockfile example
        {
          "lockfileVersion": 1,
          "workspaces": {
            "": {"name": "root", "version": "1.0.0", "dependencies": {"left-pad": "^1.0.0"}},
            "docs": {"name": "docs", "version": "0.1.0"}
          },
          "packages": {
            "left-pad": ["left-pad@npm:1.3.0", "", {"dependencies": {"repeat-string": "^1.0.0"}}, "sha512-left"],
            "repeat-string": ["repeat-string@1.0.0", "", {}, "sha512-repeat"],
            "@scope/pkg": ["@scope/pkg@github:user/repo#abcdef", "", {}, "sha512-scope"]
          }
        }
        "#;

        let file = write_lock(lock);
        let parser = BunLockfileParser;
        let entries = parser.parse(file.path()).expect("parse bun lock");

        assert_eq!(entries.len(), 5);

        let workspace = entries
            .iter()
            .find(|entry| entry.is_workspace_member && entry.name == "root")
            .expect("root workspace");
        assert_eq!(workspace.version, "1.0.0");
        assert_eq!(workspace.dependencies.len(), 1);

        let left_pad = entries
            .iter()
            .find(|entry| entry.name == "left-pad")
            .expect("left-pad entry");
        assert_eq!(left_pad.version, "1.3.0");
        assert_eq!(left_pad.dependencies.len(), 1);
        assert_eq!(left_pad.checksum.as_deref(), Some("sha512-left"));

        let scoped = entries
            .iter()
            .find(|entry| entry.name == "@scope/pkg")
            .expect("scoped package");
        assert_eq!(scoped.version, "abcdef");
        assert!(matches!(scoped.source, DependencySource::Git(_)));
    }

    #[test]
    fn rejects_future_version() {
        let lock = r#"{"lockfileVersion": 99, "packages": {}}"#;
        let file = write_lock(lock);
        let parser = BunLockfileParser;
        let err = parser
            .parse(file.path())
            .expect_err("reject future version");
        match err {
            Error::LockfileParseFailed { message, .. } => {
                assert!(message.contains("Unsupported"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn supports_expected_filenames() {
        let parser = BunLockfileParser;
        assert!(parser.supports_lockfile(Path::new("/tmp/bun.lock")));
        assert!(
            !parser.supports_lockfile(Path::new("./bun.lockb")),
            "Binary bun.lockb should not be supported"
        );
        assert!(!parser.supports_lockfile(Path::new("package-lock.json")));
    }

    #[test]
    fn rejects_binary_lockb_format() {
        use std::io::Write;
        use tempfile::TempDir;

        let lock = r#"{"lockfileVersion": 1, "packages": {}}"#;
        let dir = TempDir::new().expect("temp dir");
        let lockb_path = dir.path().join("bun.lockb");

        std::fs::write(&lockb_path, lock.as_bytes()).expect("write lockfile");

        let parser = BunLockfileParser;
        let err = parser
            .parse(&lockb_path)
            .expect_err("should reject bun.lockb");
        match err {
            Error::LockfileParseFailed { message, .. } => {
                assert!(message.contains("Binary Bun lockfile format"));
                assert!(message.contains("unsupported"));
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn parses_metadata_string_in_tuple() {
        let lock = r#"
        {
          "lockfileVersion": 1,
          "workspaces": {},
          "packages": {
            "@emmetio/css-parser": ["npm:@emmetio/css-parser@0.0.1", null, "ramya-rao-a-css-parser-370c480"]
          }
        }
        "#;

        let file = write_lock(lock);
        let parser = BunLockfileParser;
        let entries = parser.parse(file.path()).expect("parse bun lock");

        let pkg = entries
            .iter()
            .find(|entry| entry.name == "@emmetio/css-parser")
            .expect("package parsed");
        assert_eq!(pkg.version, "0.0.1");
        assert_eq!(
            pkg.checksum.as_deref(),
            Some("ramya-rao-a-css-parser-370c480")
        );
    }
}
