use crate::core::traits::LockfileParser;
use crate::core::types::{DependencyRef, DependencySource, LockfileEntry};
use crate::error::{Error, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Parser for Yarn Classic (v1.x) `yarn.lock` files.
///
/// The format is parsed line by line by a small, infallible state machine.
/// Unrecognised lines are skipped and entries without a version are dropped,
/// so malformed input degrades to fewer entries instead of an error or panic.
#[derive(Debug, Default, Clone, Copy)]
pub struct YarnClassicLockfileParser;

impl LockfileParser for YarnClassicLockfileParser {
    fn parse(&self, lockfile_path: &Path) -> Result<Vec<LockfileEntry>> {
        let contents = fs::read_to_string(lockfile_path).map_err(|source| Error::Io {
            source,
            path: Some(lockfile_path.to_path_buf()),
            operation: "reading yarn.lock".to_string(),
        })?;

        Ok(parse_yarn_lockfile(&contents))
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

/// Parse the contents of a Yarn Classic lockfile into entries.
fn parse_yarn_lockfile(contents: &str) -> Vec<LockfileEntry> {
    contents
        .lines()
        .fold(YarnClassicParser::default(), |mut parser, line| {
            parser.parse_line(line);
            parser
        })
        .finish()
}

/// Entry currently being accumulated by [`YarnClassicParser`].
#[derive(Default)]
struct PendingEntry {
    name: String,
    version: Option<String>,
    resolved: Option<String>,
    integrity: Option<String>,
    dependencies: Vec<DependencyRef>,
}

impl PendingEntry {
    fn new(name: String) -> Self {
        Self {
            name,
            ..Self::default()
        }
    }

    /// Convert into a [`LockfileEntry`], dropping entries that never declared a version.
    fn into_lockfile_entry(self) -> Option<LockfileEntry> {
        let version = self.version?;
        let source = dependency_source(self.resolved, &self.name);
        Some(LockfileEntry {
            name: self.name,
            version,
            source,
            checksum: self.integrity,
            dependencies: self.dependencies,
            is_workspace_member: false,
        })
    }
}

/// Which nested block of an entry the parser is inside.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Section {
    /// Directly under the entry header (`version`, `resolved`, ...).
    #[default]
    Properties,
    /// Inside `dependencies:` or `optionalDependencies:`.
    Dependencies,
    /// Inside any other nested block (for example `peerDependenciesMeta:`), which is ignored.
    Ignored,
}

#[derive(Default)]
struct YarnClassicParser {
    entries: Vec<LockfileEntry>,
    current: Option<PendingEntry>,
    section: Section,
    /// Indentation of the nested block's key line, used to detect when the block ends.
    section_indent: usize,
}

impl YarnClassicParser {
    fn parse_line(&mut self, line: &str) {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            return;
        }

        let indent = line.len() - line.trim_start().len();
        if indent == 0 {
            self.start_entry(trimmed);
            return;
        }

        if self.current.is_none() {
            return;
        }

        if self.section != Section::Properties && indent <= self.section_indent {
            self.section = Section::Properties;
        }

        match self.section {
            Section::Properties => self.parse_property(trimmed, indent),
            Section::Dependencies => self.push_dependency(trimmed),
            Section::Ignored => {}
        }
    }

    fn start_entry(&mut self, header: &str) {
        self.finish_current_entry();
        self.section = Section::Properties;
        self.current = header
            .strip_suffix(':')
            .and_then(package_name_from_descriptor)
            .map(PendingEntry::new);
    }

    fn parse_property(&mut self, trimmed: &str, indent: usize) {
        if let Some(block) = trimmed.strip_suffix(':') {
            self.section = match block.trim_matches('"') {
                "dependencies" | "optionalDependencies" => Section::Dependencies,
                _ => Section::Ignored,
            };
            self.section_indent = indent;
            return;
        }

        let Some(current) = self.current.as_mut() else {
            return;
        };
        let Some((key, value)) = split_key_value(trimmed) else {
            return;
        };
        let value = Some(value.to_string());
        match key {
            "version" => current.version = value,
            "resolved" => current.resolved = value,
            "integrity" => current.integrity = value,
            _ => {}
        }
    }

    fn push_dependency(&mut self, trimmed: &str) {
        let Some(current) = self.current.as_mut() else {
            return;
        };
        if let Some((name, version_req)) = split_key_value(trimmed) {
            current.dependencies.push(DependencyRef {
                name: name.to_string(),
                version_req: version_req.to_string(),
            });
        }
    }

    fn finish_current_entry(&mut self) {
        if let Some(entry) = self
            .current
            .take()
            .and_then(PendingEntry::into_lockfile_entry)
        {
            self.entries.push(entry);
        }
    }

    fn finish(mut self) -> Vec<LockfileEntry> {
        self.finish_current_entry();
        self.entries
    }
}

/// Split a `key value` line where either side may be double-quoted.
///
/// Returns `None` when the line has no value or either side is empty.
fn split_key_value(trimmed: &str) -> Option<(&str, &str)> {
    let (key, rest) = match trimmed.strip_prefix('"') {
        Some(quoted) => quoted.split_once('"')?,
        None => trimmed.split_once(char::is_whitespace)?,
    };
    let value = rest.trim().trim_matches('"');
    (!key.is_empty() && !value.is_empty()).then_some((key, value))
}

/// Derive the dependency source from the `resolved` URL.
fn dependency_source(resolved: Option<String>, name: &str) -> DependencySource {
    match resolved {
        Some(url) if url.starts_with("git+") || url.contains("://github.com/") => {
            DependencySource::Git(url)
        }
        Some(url) if url.starts_with("file:") => {
            DependencySource::Path(PathBuf::from(url.trim_start_matches("file:")))
        }
        Some(url) => DependencySource::Registry(url),
        None => DependencySource::Registry(format!("npm:{name}")),
    }
}

/// Extract the package name from an entry header such as
/// `"@babel/core@^7.0.0", "@babel/core@^7.22.0"` or `left-pad@^1.3.0`.
fn package_name_from_descriptor(header: &str) -> Option<String> {
    let first = header.split(',').next()?.trim().trim_matches('"');
    let name = match first.strip_prefix('@') {
        Some(rest) => rest
            .find('@')
            .map_or(first, |second_at| &first[..=second_at]),
        None => first.split('@').next().unwrap_or(first),
    };
    (!name.is_empty()).then(|| name.to_string())
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

    fn entry<'a>(entries: &'a [LockfileEntry], name: &str) -> &'a LockfileEntry {
        entries
            .iter()
            .find(|e| e.name == name)
            .unwrap_or_else(|| panic!("missing entry {name}"))
    }

    #[test]
    fn parses_lockfile_without_v1_header() {
        // `yarn_lock_parser` 0.12 required `# yarn lockfile v1` on exactly the
        // second line and rejected anything else, which sent these lockfiles
        // down the old fallback path. The parser must handle them directly.
        let yarn_lock = r#"lodash@^4.17.21:
  version "4.17.21"
  resolved "https://registry.yarnpkg.com/lodash/-/lodash-4.17.21.tgz"
  integrity sha512-v2kDEe57lecTulaDIuNTPy3Ry4gLGJ6Z1O3vE1krgXZNrsQ+LFTGHVxVjcXPs17LhbZVGedAJv8XZ1tvj5FvSg==
"#;

        let entries = parse_yarn_lockfile(yarn_lock);
        assert_eq!(entries.len(), 1);
        assert_eq!(entry(&entries, "lodash").version, "4.17.21");
    }

    /// Lines of a realistic Yarn 1.22 lockfile, joined with CRLF by the test.
    const REALISTIC_YARN_V1_LINES: &[&str] = &[
        "# THIS IS AN AUTOGENERATED FILE. DO NOT EDIT THIS FILE DIRECTLY.",
        "# yarn lockfile v1",
        "",
        "",
        "\"@babel/code-frame@^7.0.0\", \"@babel/code-frame@^7.22.5\":",
        "  version \"7.22.5\"",
        "  resolved \"https://registry.yarnpkg.com/@babel/code-frame/-/code-frame-7.22.5.tgz#234d98e1551960604f1246e6475891a570ad5658\"",
        "  integrity sha512-Xmwn266vad+6DAqEB2A6V/CcZVp62BbwVmcOJc2RPuwih1kw02TjQvWVWlcKGbBPd+8/0V5DEkOcizRGYsspYQ==",
        "  dependencies:",
        "    \"@babel/highlight\" \"^7.22.5\"",
        "",
        "chokidar@^3.5.3:",
        "  version \"3.5.3\"",
        "  resolved \"https://registry.yarnpkg.com/chokidar/-/chokidar-3.5.3.tgz\"",
        "  dependencies:",
        "    anymatch \"~3.1.2\"",
        "    glob-parent \"~5.1.2\"",
        "  optionalDependencies:",
        "    fsevents \"~2.3.2\"",
        "",
        "\"string-width-cjs@npm:string-width@^4.2.0\":",
        "  version \"4.2.3\"",
        "  resolved \"https://registry.yarnpkg.com/string-width/-/string-width-4.2.3.tgz\"",
        "",
        "\"my-fork@git+https://github.com/acme/my-fork.git#abc123\":",
        "  version \"1.0.0\"",
        "  resolved \"git+https://github.com/acme/my-fork.git#abc123\"",
        "",
        "\"local-pkg@file:./packages/local\":",
        "  version \"0.0.1\"",
        "  resolved \"file:./packages/local\"",
        "",
    ];

    /// Realistic Yarn 1.22 output: quoted scoped dependency names,
    /// `optionalDependencies`, alias descriptors, git and file sources, and
    /// CRLF line endings.
    #[test]
    fn parses_realistic_yarn_v1_lockfile() {
        let yarn_lock = REALISTIC_YARN_V1_LINES.join("\r\n");

        let entries = parse_yarn_lockfile(&yarn_lock);
        assert_eq!(entries.len(), 5);

        let code_frame = entry(&entries, "@babel/code-frame");
        assert_eq!(code_frame.version, "7.22.5");
        assert!(
            code_frame
                .checksum
                .as_deref()
                .is_some_and(|c| c.starts_with("sha512-"))
        );
        assert_eq!(code_frame.dependencies.len(), 1);
        assert_eq!(code_frame.dependencies[0].name, "@babel/highlight");
        assert_eq!(code_frame.dependencies[0].version_req, "^7.22.5");

        let chokidar = entry(&entries, "chokidar");
        let deps: Vec<(&str, &str)> = chokidar
            .dependencies
            .iter()
            .map(|d| (d.name.as_str(), d.version_req.as_str()))
            .collect();
        assert_eq!(
            deps,
            [
                ("anymatch", "~3.1.2"),
                ("glob-parent", "~5.1.2"),
                ("fsevents", "~2.3.2")
            ]
        );
        assert!(chokidar.checksum.is_none());

        assert_eq!(entry(&entries, "string-width-cjs").version, "4.2.3");
        assert!(matches!(
            entry(&entries, "my-fork").source,
            DependencySource::Git(_)
        ));
        assert_eq!(
            entry(&entries, "local-pkg").source,
            DependencySource::Path(PathBuf::from("./packages/local"))
        );
    }

    #[test]
    fn ignores_unknown_nested_blocks_and_entries_without_version() {
        let yarn_lock = r#"
broken@^1.0.0:
  resolved "https://example.com/broken.tgz"

pkg@^1.0.0:
  version "1.0.0"
  peerDependenciesMeta:
    typescript:
      optional true
  dependencies:
    dep "^2.0.0"
  integrity sha512-after-nested
"#;

        let entries = parse_yarn_lockfile(yarn_lock);
        assert_eq!(entries.len(), 1);
        let pkg = entry(&entries, "pkg");
        assert_eq!(pkg.dependencies.len(), 1);
        assert_eq!(pkg.dependencies[0].name, "dep");
        assert_eq!(pkg.checksum.as_deref(), Some("sha512-after-nested"));
    }

    /// Malformed input must never panic (release builds use `panic = "abort"`).
    #[test]
    fn malformed_input_never_panics() {
        let samples = [
            "",
            ":",
            "\"",
            "@:",
            "@@@:\n  version",
            "\"unterminated:\n  \"dep\n  version \"\"",
            "pkg@1:\n  dependencies:\n    \"\" \"1\"\n    lonely\n",
            "  version \"1.0.0\"\n",
            "\u{feff}pkg@1:\n\tversion \"1\"\n",
            "pkg@1:\n  version \"1\"\n  dependencies:\n",
        ];

        for sample in samples {
            let _ = parse_yarn_lockfile(sample);
        }

        let realistic = "\"@a/b@^1\", \"@a/b@^1.1\":\n  version \"1.1.0\"\n  dependencies:\n    \"@c/d\" \"^2\"\n";
        for end in (0..=realistic.len()).filter(|i| realistic.is_char_boundary(*i)) {
            let _ = parse_yarn_lockfile(&realistic[..end]);
        }
    }
}
