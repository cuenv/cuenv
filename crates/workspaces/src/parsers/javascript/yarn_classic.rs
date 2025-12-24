use crate::core::traits::LockfileParser;
use crate::core::types::{DependencyRef, DependencySource, LockfileEntry};
use crate::error::{Error, Result};
use std::fs;
use std::panic;
use std::path::{Path, PathBuf};

type LockfileDetail = (Option<String>, Option<String>, Vec<DependencyRef>);

/// Parser for Yarn Classic (v1.x) `yarn.lock` files.
#[derive(Debug, Default, Clone, Copy)]
pub struct YarnClassicLockfileParser;

impl LockfileParser for YarnClassicLockfileParser {
    fn parse(&self, lockfile_path: &Path) -> Result<Vec<LockfileEntry>> {
        let contents = fs::read_to_string(lockfile_path).map_err(|source| Error::Io {
            source,
            path: Some(lockfile_path.to_path_buf()),
            operation: "reading yarn.lock".to_string(),
        })?;

        // Try to use yarn_lock_parser for name and version extraction.
        // If it fails (which can happen on some valid yarn.lock files), fall back to manual parsing.
        // Note: yarn_lock_parser only provides name and version, not resolved/integrity/dependencies.
        // We use catch_unwind because yarn_lock_parser can panic on some valid lockfiles.
        let entries = panic::catch_unwind(panic::AssertUnwindSafe(|| {
            yarn_lock_parser::parse_str(&contents)
        }))
        .ok()
        .and_then(
            |r: std::result::Result<
                yarn_lock_parser::Lockfile<'_>,
                yarn_lock_parser::YarnLockError,
            >| r.ok(),
        )
        .map(|lockfile| {
            let parsed_entries = lockfile.entries;
            let detailed_entries = parse_lockfile_details(&contents);
            let mut result = Vec::new();

            for (i, basic_entry) in parsed_entries.iter().enumerate() {
                let name = basic_entry.name.to_string();
                let version = basic_entry.version.to_string();

                let (resolved, integrity, dependencies) = detailed_entries.get(i).map_or_else(
                    || (None, None, Vec::new()),
                    |(r, i, d): &LockfileDetail| (r.clone(), i.clone(), d.clone()),
                );

                result.push(build_lockfile_entry(
                    name,
                    version,
                    resolved,
                    integrity,
                    dependencies,
                ));
            }

            result
        })
        .map_or_else(
            || {
                // Fall back to fully manual parsing if yarn_lock_parser fails or panics
                parse_yarn_lockfile_fully(&contents, lockfile_path)
            },
            |entries| entries,
        );

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

        // Read a small prefix of the file to distinguish Yarn v1 from v2+
        // Yarn Classic (v1) uses a header like "# yarn lockfile v1"
        // Yarn Modern (v2+) uses YAML with structures like "__metadata:"
        if let Ok(contents) = fs::read_to_string(path) {
            // Yarn Classic v1 has a specific header comment
            if contents.contains("# yarn lockfile v1") {
                return true;
            }

            // If it looks like YAML with __metadata, it's Modern (not Classic)
            if contents.contains("__metadata:") {
                return false;
            }

            // If it contains "@npm:" it's Yarn Modern format (not Classic)
            if contents.contains("@npm:") {
                return false;
            }

            // If we see v1-style unquoted package descriptors without Modern indicators,
            // it's likely Classic
            for line in contents.lines().take(30) {
                // v1 has descriptors like: lodash@^4.17.21:
                // Modern has descriptors like: "lodash@npm:^4.17.21":
                if !line.starts_with(' ')
                    && !line.starts_with('\t')
                    && !line.starts_with('#')
                    && line.contains('@')
                    && line.ends_with(':')
                    && !line.starts_with('"')
                // Modern uses quoted keys
                {
                    return true;
                }
            }
        }

        // Default to false if we can't determine - let Yarn Modern try
        false
    }

    fn lockfile_name(&self) -> &'static str {
        "yarn.lock"
    }
}

/// Build a `LockfileEntry` from parsed components
#[allow(clippy::option_if_let_else)] // Complex parsing with nested conditionals - imperative is clearer
fn build_lockfile_entry(
    name: String,
    version: String,
    resolved: Option<String>,
    integrity: Option<String>,
    dependencies: Vec<DependencyRef>,
) -> LockfileEntry {
    let source = if let Some(resolved_url) = resolved {
        if resolved_url.starts_with("git+") || resolved_url.contains("://github.com/") {
            DependencySource::Git(resolved_url)
        } else if resolved_url.starts_with("file:") {
            DependencySource::Path(PathBuf::from(resolved_url.trim_start_matches("file:")))
        } else {
            DependencySource::Registry(resolved_url)
        }
    } else {
        DependencySource::Registry(format!("npm:{name}"))
    };

    LockfileEntry {
        name,
        version,
        source,
        checksum: integrity,
        dependencies,
        is_workspace_member: false,
    }
}

/// Parse additional details from Yarn Classic lockfile that `yarn_lock_parser` doesn't provide.
/// Returns a vector of (`resolved_url`, `integrity`, `dependencies`) in the same order as entries appear.
fn parse_lockfile_details(contents: &str) -> Vec<LockfileDetail> {
    let mut details = Vec::new();
    let mut current_resolved: Option<String> = None;
    let mut current_integrity: Option<String> = None;
    let mut current_dependencies = Vec::new();
    let mut in_entry = false;

    for line in contents.lines() {
        let trimmed = line.trim();

        // Skip comments and empty lines
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Check if this is a package header (no leading whitespace)
        if !line.starts_with(' ') && !line.starts_with('\t') {
            // Save the previous entry if exists
            if in_entry {
                details.push((
                    current_resolved.take(),
                    current_integrity.take(),
                    std::mem::take(&mut current_dependencies),
                ));
            }
            in_entry = true;
        } else if in_entry {
            // Parse entry properties
            if let Some(resolved) = trimmed.strip_prefix("resolved ") {
                current_resolved = Some(resolved.trim_matches('"').to_string());
            } else if let Some(integrity) = trimmed.strip_prefix("integrity ") {
                current_integrity = Some(integrity.trim_matches('"').to_string());
            } else if trimmed.starts_with("dependencies:")
                || trimmed.starts_with("optionalDependencies:")
            {
                // Dependencies section marker - will be parsed in next lines
            } else if trimmed.contains(' ')
                && !trimmed.starts_with('"')
                && !trimmed.starts_with("version ")
            {
                // Dependency line: name "version"
                let parts: Vec<&str> = trimmed.splitn(2, ' ').collect();
                if parts.len() == 2 {
                    let dep_name = parts[0].trim();
                    let dep_version = parts[1].trim_matches('"');
                    if !dep_name.is_empty() && !dep_version.is_empty() {
                        current_dependencies.push(DependencyRef {
                            name: dep_name.to_string(),
                            version_req: dep_version.to_string(),
                        });
                    }
                }
            }
        }
    }

    // Save the last entry
    if in_entry {
        details.push((current_resolved, current_integrity, current_dependencies));
    }

    details
}

/// Fully manual parser for Yarn Classic lockfiles (used as fallback when `yarn_lock_parser` fails).
/// This parses everything including name, version, resolved, integrity, and dependencies.
#[allow(clippy::cognitive_complexity)]
#[allow(clippy::option_if_let_else)] // Complex parsing with nested conditionals - imperative is clearer
fn parse_yarn_lockfile_fully(contents: &str, _lockfile_path: &Path) -> Vec<LockfileEntry> {
    let mut entries = Vec::new();
    let mut current_name: Option<String> = None;
    let mut current_version: Option<String> = None;
    let mut current_resolved: Option<String> = None;
    let mut current_integrity: Option<String> = None;
    let mut current_dependencies = Vec::new();
    let mut in_entry = false;

    for line in contents.lines() {
        let trimmed = line.trim();

        // Skip comments and empty lines
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Check if this is a package header (no leading whitespace)
        if !line.starts_with(' ') && !line.starts_with('\t') {
            // Save the previous entry if exists
            if in_entry
                && let (Some(name), Some(version)) = (current_name.take(), current_version.take())
            {
                entries.push(build_lockfile_entry(
                    name,
                    version,
                    current_resolved.take(),
                    current_integrity.take(),
                    std::mem::take(&mut current_dependencies),
                ));
            }

            // Parse the package name from the descriptor
            // Format: "package-name@^1.0.0" or package-name@^1.0.0:
            let descriptor = trimmed.trim_end_matches(':').trim_matches('"');
            let first_descriptor = descriptor.split(',').next().unwrap_or(descriptor).trim();

            let name = if let Some(rest) = first_descriptor.strip_prefix('@') {
                // Scoped package: @scope/name@version
                if let Some(second_at) = rest.find('@') {
                    format!("@{}", &rest[..second_at])
                } else {
                    first_descriptor.to_string()
                }
            } else {
                // Regular package: name@version
                first_descriptor
                    .split('@')
                    .next()
                    .unwrap_or(first_descriptor)
                    .to_string()
            };

            current_name = Some(name);
            in_entry = true;
        } else if in_entry {
            // Parse entry properties
            if let Some(version) = trimmed.strip_prefix("version ") {
                current_version = Some(version.trim_matches('"').to_string());
            } else if let Some(resolved) = trimmed.strip_prefix("resolved ") {
                current_resolved = Some(resolved.trim_matches('"').to_string());
            } else if let Some(integrity) = trimmed.strip_prefix("integrity ") {
                current_integrity = Some(integrity.trim_matches('"').to_string());
            } else if trimmed.starts_with("dependencies:")
                || trimmed.starts_with("optionalDependencies:")
            {
                // Dependencies section marker
            } else if trimmed.contains(' ') && !trimmed.starts_with('"') {
                // Dependency line: name "version"
                let parts: Vec<&str> = trimmed.splitn(2, ' ').collect();
                if parts.len() == 2 {
                    let dep_name = parts[0].trim();
                    let dep_version = parts[1].trim_matches('"');
                    if !dep_name.is_empty() && !dep_version.is_empty() {
                        current_dependencies.push(DependencyRef {
                            name: dep_name.to_string(),
                            version_req: dep_version.to_string(),
                        });
                    }
                }
            }
        }
    }

    // Save the last entry
    if in_entry && let (Some(name), Some(version)) = (current_name, current_version) {
        entries.push(build_lockfile_entry(
            name,
            version,
            current_resolved,
            current_integrity,
            current_dependencies,
        ));
    }

    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn parses_basic_yarn_lock() {
        let yarn_lock = r#"# THIS IS AN AUTOGENERATED FILE. DO NOT EDIT THIS FILE DIRECTLY.
# yarn lockfile v1

left-pad@^1.3.0:
  version "1.3.0"
  resolved "https://registry.yarnpkg.com/left-pad/-/left-pad-1.3.0.tgz"
  integrity sha512-test123

react@^18.0.0:
  version "18.2.0"
  resolved "https://registry.yarnpkg.com/react/-/react-18.2.0.tgz"
  dependencies:
    loose-envify "^1.1.0"
"#;

        let mut file = NamedTempFile::new().unwrap();
        file.write_all(yarn_lock.as_bytes()).unwrap();

        let parser = YarnClassicLockfileParser;
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
    fn parses_scoped_packages() {
        let yarn_lock = r#"
"@babel/core@^7.22.0":
  version "7.22.5"
  resolved "https://registry.yarnpkg.com/@babel/core/-/core-7.22.5.tgz"
  integrity sha512-abc123
"#;

        let mut file = NamedTempFile::new().unwrap();
        file.write_all(yarn_lock.as_bytes()).unwrap();

        let parser = YarnClassicLockfileParser;
        let entries = parser.parse(file.path()).unwrap();

        assert!(!entries.is_empty());
        let babel = entries.iter().find(|e| e.name == "@babel/core");
        assert!(babel.is_some());
        let babel = babel.unwrap();
        assert_eq!(babel.version, "7.22.5");
        assert_eq!(babel.checksum.as_deref(), Some("sha512-abc123"));
    }

    #[test]
    fn parses_multiple_descriptors_same_version() {
        let yarn_lock = r#"
left-pad@^1.3.0, left-pad@~1.3.0:
  version "1.3.0"
  resolved "https://registry.yarnpkg.com/left-pad/-/left-pad-1.3.0.tgz"
  integrity sha512-test123
  dependencies:
    repeat-string "^1.0.0"

repeat-string@^1.0.0:
  version "1.6.1"
  resolved "https://registry.yarnpkg.com/repeat-string/-/repeat-string-1.6.1.tgz"
"#;

        let mut file = NamedTempFile::new().unwrap();
        file.write_all(yarn_lock.as_bytes()).unwrap();

        let parser = YarnClassicLockfileParser;
        let entries = parser.parse(file.path()).unwrap();

        // Should have 2 entries (left-pad and repeat-string)
        // Multiple descriptors should result in a single entry
        assert_eq!(entries.len(), 2);

        let left_pad = entries.iter().find(|e| e.name == "left-pad");
        assert!(left_pad.is_some());
        let left_pad = left_pad.unwrap();
        assert_eq!(left_pad.version, "1.3.0");
        assert_eq!(left_pad.dependencies.len(), 1);
        assert_eq!(left_pad.dependencies[0].name, "repeat-string");

        let repeat_string = entries.iter().find(|e| e.name == "repeat-string");
        assert!(repeat_string.is_some());
        assert_eq!(repeat_string.unwrap().version, "1.6.1");
    }

    #[test]
    fn supports_expected_filename() {
        let parser = YarnClassicLockfileParser;
        assert!(parser.supports_lockfile(Path::new("/tmp/yarn.lock")));
        assert!(!parser.supports_lockfile(Path::new("package-lock.json")));
    }
}
