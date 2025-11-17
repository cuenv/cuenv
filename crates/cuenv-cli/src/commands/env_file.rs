use cuenv_core::{Error, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Status for env.cue detection
#[derive(Debug)]
pub enum EnvFileStatus {
    /// No env.cue present in the directory
    Missing,
    /// env.cue exists but does not match the expected package
    PackageMismatch { found_package: Option<String> },
    /// env.cue exists and matches the expected package. Contains canonical directory path.
    Match(PathBuf),
}

/// Locate env.cue in `path` and ensure it declares the expected package.
pub fn find_env_file(path: &Path, expected_package: &str) -> Result<EnvFileStatus> {
    let directory = if path.is_absolute() {
        path.to_path_buf()
    } else {
        PathBuf::from(path)
    };

    let env_file = directory.join("env.cue");
    if !env_file.exists() {
        return Ok(EnvFileStatus::Missing);
    }

    let package_name = detect_package_name(&env_file)?;
    if package_name.as_deref() != Some(expected_package) {
        return Ok(EnvFileStatus::PackageMismatch {
            found_package: package_name,
        });
    }

    let canonical = directory
        .canonicalize()
        .map_err(|e| Error::configuration(format!("Failed to canonicalize path: {e}")))?;

    Ok(EnvFileStatus::Match(canonical))
}

fn detect_package_name(env_file: &Path) -> Result<Option<String>> {
    let contents = fs::read_to_string(env_file).map_err(|e| {
        Error::configuration(format!(
            "Failed to read env.cue ({}): {e}",
            env_file.display()
        ))
    })?;

    let cleaned = strip_comments(contents.trim_start_matches('\u{feff}'));

    for line in cleaned.lines() {
        let trimmed = line.trim();

        if trimmed.is_empty() {
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("package ") {
            if let Some(name) = rest.split_whitespace().next()
                && !name.is_empty()
            {
                return Ok(Some(name.to_string()));
            }
            return Ok(None);
        }
        break;
    }

    Ok(None)
}

fn strip_comments(source: &str) -> String {
    let mut result = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '/' {
            match chars.peek() {
                Some('/') => {
                    chars.next();
                    for next in chars.by_ref() {
                        if next == '\n' {
                            result.push('\n');
                            break;
                        }
                    }
                    continue;
                }
                Some('*') => {
                    chars.next();
                    let mut prev = '\0';
                    for next in chars.by_ref() {
                        if prev == '*' && next == '/' {
                            break;
                        }
                        prev = next;
                    }
                    continue;
                }
                _ => {}
            }
        }

        result.push(ch);
    }

    result
}

#[cfg(test)]
mod tests {
    use super::{EnvFileStatus, detect_package_name, find_env_file, strip_comments};
    use std::fs;
    use std::io::Write;
    use std::path::Path;
    use tempfile::{NamedTempFile, TempDir};

    #[test]
    fn strip_comments_removes_line_and_block_comments() {
        let source = r"
// line comment
/* block
comment */
package cuenv // inline
        ";
        let cleaned = strip_comments(source);
        assert!(cleaned.contains("package cuenv"));
        assert!(!cleaned.contains("line comment"));
        assert!(!cleaned.contains("block"));
    }

    #[test]
    fn detect_package_name_finds_package() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "// comment\npackage cuenv // inline\n\nenv: {{}}").unwrap();

        let package = detect_package_name(Path::new(file.path())).unwrap();
        assert_eq!(package, Some("cuenv".to_string()));
    }

    #[test]
    fn detect_package_name_handles_missing() {
        let mut file = NamedTempFile::new().unwrap();
        writeln!(file, "// only comments").unwrap();
        let package = detect_package_name(Path::new(file.path())).unwrap();
        assert!(package.is_none());
    }

    #[test]
    fn find_env_file_detects_package_mismatch() {
        let temp_dir = TempDir::new().unwrap();
        fs::write(temp_dir.path().join("env.cue"), "package other\n").unwrap();

        let status = find_env_file(temp_dir.path(), "cuenv").unwrap();
        match status {
            EnvFileStatus::PackageMismatch { found_package } => {
                assert_eq!(found_package.as_deref(), Some("other"));
            }
            _ => panic!("Expected package mismatch status"),
        }
    }
}
