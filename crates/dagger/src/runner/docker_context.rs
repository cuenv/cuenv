use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use cuenv_core::{Error, Result};

pub(super) fn excludes(project: &Path, dockerfile: &Path) -> Result<Vec<String>> {
    let mut specific = dockerfile.as_os_str().to_os_string();
    specific.push(".dockerignore");
    let specific = PathBuf::from(specific);
    let root_ignore = Path::new(".dockerignore");

    // Dagger's dockerBuild uses the Dockerfile-specific ignore file unless
    // it is missing or empty.
    let (rules, ignore_file) = match read_ignore(project, &specific)? {
        Some(rules) if !rules.is_empty() => (rules, specific.as_path()),
        _ => (
            read_ignore(project, root_ignore)?.unwrap_or_default(),
            root_ignore,
        ),
    };
    let mut patterns = parse_patterns(&rules)?;
    if !patterns.is_empty() {
        // dockerBuild reads these from the imported Directory before applying
        // the ignore rules again to files available to the build.
        patterns.push(allow_control_file(dockerfile)?);
        patterns.push(allow_control_file(ignore_file)?);
    }
    Ok(patterns)
}

fn read_ignore(project: &Path, name: &Path) -> Result<Option<String>> {
    let path = project.join(name);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(Error::io_with_path(
                "inspect Docker ignore file",
                path,
                error,
            ));
        }
    };
    if !metadata.is_file() {
        return Err(Error::configuration(format!(
            "Docker ignore file must be a regular file: {}",
            path.display()
        )));
    }
    let root = project.canonicalize().map_err(|error| {
        Error::io_with_path("resolve Docker build context", project.to_path_buf(), error)
    })?;
    let resolved = path
        .canonicalize()
        .map_err(|error| Error::io_with_path("resolve Docker ignore file", path.clone(), error))?;
    if !resolved.starts_with(root) {
        return Err(Error::configuration(format!(
            "Docker ignore file is outside the context: {}",
            path.display()
        )));
    }
    fs::read_to_string(&resolved)
        .map(Some)
        .map_err(|error| Error::io_with_path("read Docker ignore file", resolved, error))
}

fn parse_patterns(contents: &str) -> Result<Vec<String>> {
    // Match the normalization performed by Moby's ignorefile.ReadAll before
    // sending patterns to Dagger's caller-side filesync matcher.
    let contents = contents.strip_prefix('\u{feff}').unwrap_or(contents);
    let mut patterns = Vec::new();
    for raw in contents.lines() {
        if raw.starts_with('#') || raw.trim().is_empty() {
            continue;
        }
        let pattern = raw.trim();
        let (pattern, inverted) = pattern
            .strip_prefix('!')
            .map_or((pattern, false), |pattern| (pattern.trim(), true));
        if pattern.is_empty() {
            return Err(Error::configuration(
                "Docker ignore file contains '!' without a pattern",
            ));
        }
        let cleaned = relative_pattern(pattern);
        // Docker ignores the historical "." pattern. The host directory API
        // would otherwise interpret it as "*".
        if !cleaned.is_empty() {
            patterns.push(if inverted {
                format!("!{cleaned}")
            } else {
                cleaned
            });
        }
    }
    Ok(patterns)
}

fn relative_pattern(pattern: &str) -> String {
    let absolute = pattern.starts_with('/');
    let mut parts = Vec::new();
    for part in pattern.split('/') {
        match part {
            "" | "." => {}
            ".." if parts.last().is_some_and(|last| *last != "..") => {
                parts.pop();
            }
            ".." if absolute => {}
            part => parts.push(part),
        }
    }
    parts.join("/")
}

fn allow_control_file(path: &Path) -> Result<String> {
    let path = path.to_str().ok_or_else(|| {
        Error::configuration(format!(
            "Docker build path is not valid UTF-8: {}",
            path.display()
        ))
    })?;
    let mut pattern = "!".to_string();
    for character in relative_pattern(path).chars() {
        if matches!(character, '*' | '?' | '[' | ']' | '\\') {
            pattern.push('\\');
        }
        pattern.push(character);
    }
    Ok(pattern)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_docker_rules_without_altering_order() -> Result<()> {
        let patterns = parse_patterns(
            "\u{feff}# comment\r\n/dir//nested/../secret\n ! /dir/allowed \n**/credentials?\n . \n #literal\n",
        )?;
        assert_eq!(
            patterns,
            ["dir/secret", "!dir/allowed", "**/credentials?", "#literal"]
        );
        assert!(parse_patterns("! ").is_err());
        Ok(())
    }

    #[test]
    fn prefers_specific_rules_and_preserves_metadata() -> Result<()> {
        let project = tempfile::tempdir()?;
        let dockerfile = Path::new("images/[dev].Dockerfile");
        fs::create_dir(project.path().join("images"))?;
        fs::write(project.path().join(".dockerignore"), "root-secret\n")?;
        let specific = project.path().join("images/[dev].Dockerfile.dockerignore");
        fs::write(&specific, "private\nimages\n")?;
        assert_eq!(
            excludes(project.path(), dockerfile)?,
            [
                "private",
                "images",
                r"!images/\[dev\].Dockerfile",
                r"!images/\[dev\].Dockerfile.dockerignore",
            ]
        );

        fs::write(specific, "")?;
        assert_eq!(
            excludes(project.path(), dockerfile)?,
            [
                "root-secret",
                r"!images/\[dev\].Dockerfile",
                "!.dockerignore"
            ]
        );
        Ok(())
    }
}
