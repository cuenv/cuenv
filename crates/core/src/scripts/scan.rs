//! Finding the scripts of a project's `scriptDirs`.
//!
//! Each file directly inside a listed directory whose interpreter can be told
//! from its first line or its extension is a script, named after the file.
//! Scanning reads directory entries and each file's first line; it never runs
//! anything.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::Deserialize;
use serde_json::Value;

/// How much of a file is read to find its shebang.
const FIRST_LINE_LIMIT: u64 = 512;

/// One directory of `scriptDirs`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScriptDirectory {
    /// The directory, relative to the project.
    pub path: String,
    /// Prepended to each file stem to name its script.
    #[serde(default)]
    pub prefix: String,
    /// File-name globs that are never scripts.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// Script names by file name, over prefix and stem.
    #[serde(default)]
    pub rename: BTreeMap<String, String>,
    /// Whether the directory's scripts run hermetically, as written in CUE.
    #[serde(default)]
    pub hermetic: Option<Value>,
    /// Inputs of each script after the script itself.
    #[serde(default)]
    pub inputs: Vec<Value>,
    /// Outputs of each script.
    #[serde(default)]
    pub outputs: Option<Value>,
    /// Cache policy of each script.
    #[serde(default)]
    pub cache: Option<Value>,
}

/// What a script's first line or extension says runs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Detected {
    /// A supported interpreter, by the program that runs it.
    Program(String),
    /// A shebang naming a program cuenv does not run scripts with.
    Unsupported(String),
    /// A shebang-less file whose extension several interpreters share.
    Ambiguous,
}

/// A script found in a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    /// Relative to the project directory, with `/` separators.
    pub file: String,
    /// What runs it.
    pub detected: Detected,
}

/// Every script of a project, by name, and why any file was left out.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Scan {
    /// The scripts, by name.
    pub scripts: BTreeMap<String, Found>,
    /// Two or more files that would share a name; none of them is a script
    /// until `rename` tells them apart.
    pub clashes: Vec<String>,
}

/// Scan every directory of `scriptDirs` under `project`.
#[must_use]
pub fn scan(project: &Path, directories: &[ScriptDirectory]) -> Scan {
    let mut named: BTreeMap<String, Vec<Found>> = BTreeMap::new();
    for directory in directories {
        for (name, found) in scan_directory(project, directory) {
            named.entry(name).or_default().push(found);
        }
    }
    let mut result = Scan::default();
    for (name, mut found) in named {
        if found.len() == 1 {
            if let Some(found) = found.pop() {
                result.scripts.insert(name, found);
            }
            continue;
        }
        let files = found
            .iter()
            .map(|found| format!("`{}`", found.file))
            .collect::<Vec<_>>()
            .join(" and ");
        result.clashes.push(format!(
            "{files} would both be script `{name}`; give one another name with `rename` in `scriptDirs`, or exclude one"
        ));
    }
    result
}

/// The program for an interpreter name from CUE or a shebang.
#[must_use]
pub fn supported(name: &str) -> bool {
    matches!(
        name,
        "bash" | "sh" | "fish" | "nu" | "bun" | "python" | "python3"
    ) || is_versioned_python(name)
}

/// `python3.12` and the like.
fn is_versioned_python(name: &str) -> bool {
    name.strip_prefix("python3.")
        .is_some_and(|minor| !minor.is_empty() && minor.bytes().all(|byte| byte.is_ascii_digit()))
}

/// The program a shebang line names, skipping `env` and its options.
fn shebang_program(line: &str) -> Option<String> {
    let mut words = line.strip_prefix("#!")?.split_whitespace();
    let program = basename(words.next()?);
    if program != "env" {
        return Some(program.to_string());
    }
    words
        .find(|word| !word.starts_with('-') && !word.contains('='))
        .map(|word| basename(word).to_string())
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The shebang, then an extension only one supported interpreter uses.
/// `None` when the file carries no sign of being a script.
fn detect(file_name: &str, first_line: &str) -> Option<Detected> {
    if let Some(program) = shebang_program(first_line) {
        return Some(if supported(&program) {
            Detected::Program(program)
        } else {
            Detected::Unsupported(program)
        });
    }
    let extension = Path::new(file_name).extension()?.to_str()?;
    match extension {
        "bash" => Some(Detected::Program("bash".to_string())),
        "fish" => Some(Detected::Program("fish".to_string())),
        "nu" => Some(Detected::Program("nu".to_string())),
        "py" => Some(Detected::Program("python3".to_string())),
        "sh" | "js" | "mjs" | "cjs" | "jsx" | "ts" | "mts" | "cts" | "tsx" => {
            Some(Detected::Ambiguous)
        }
        _ => None,
    }
}

/// Whether a script name is one a task graph reads as a single task: no
/// dots, which would name a group's child.
fn is_script_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

/// The scripts directly inside one directory, by name, sorted by file name.
/// A directory that is missing or leaves the project yields nothing.
fn scan_directory(project: &Path, directory: &ScriptDirectory) -> Vec<(String, Found)> {
    let Some((project, location)) = inside(project, &project.join(&directory.path)) else {
        tracing::warn!(
            path = %directory.path,
            "scriptDirs directory does not exist inside the project; no scripts were found in it"
        );
        return Vec::new();
    };
    if !location.is_dir() {
        tracing::warn!(path = %directory.path, "scriptDirs path is not a directory");
        return Vec::new();
    }
    let excluded = exclusions(&directory.exclude);
    let Ok(entries) = fs::read_dir(&location) else {
        tracing::warn!(path = %directory.path, "scriptDirs directory could not be read");
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(std::result::Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| !name.starts_with('.') && !excluded.is_match(name))
        .collect();
    names.sort();
    names
        .into_iter()
        .filter_map(|file_name| {
            let file = location.join(&file_name);
            // A symbolic link is followed only to a file of the project.
            let (_, target) = inside(&project, &file)?;
            if !target.is_file() {
                return None;
            }
            let detected = detect(&file_name, &first_line(&target))?;
            let name = match directory.rename.get(&file_name) {
                Some(name) => name.clone(),
                None => format!(
                    "{}{}",
                    directory.prefix,
                    Path::new(&file_name).file_stem()?.to_str()?
                ),
            };
            if !is_script_name(&name) {
                tracing::warn!(
                    file = %file_name,
                    "a scriptDirs file name is not a script name (letters, digits, `_` and `-`); rename it with `rename`"
                );
                return None;
            }
            let relative = file.strip_prefix(&project).ok()?;
            Some((
                name,
                Found {
                    file: slash_path(relative),
                    detected,
                },
            ))
        })
        .collect()
}

/// `path`, canonical, when it exists inside `project`; also returns the
/// canonical project directory.
fn inside(project: &Path, path: &Path) -> Option<(PathBuf, PathBuf)> {
    let project = project.canonicalize().ok()?;
    let path = path.canonicalize().ok()?;
    path.starts_with(&project).then_some((project, path))
}

fn exclusions(patterns: &[String]) -> GlobSet {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        match Glob::new(pattern) {
            Ok(glob) => {
                builder.add(glob);
            }
            Err(error) => {
                tracing::warn!(%pattern, %error, "scriptDirs exclude pattern is invalid");
            }
        }
    }
    builder.build().unwrap_or_else(|_| GlobSet::empty())
}

/// The first line of a file, or nothing when it cannot be read.
fn first_line(file: &Path) -> String {
    let mut buffer = Vec::new();
    let read = fs::File::open(file)
        .and_then(|handle| handle.take(FIRST_LINE_LIMIT).read_to_end(&mut buffer));
    if read.is_err() {
        return String::new();
    }
    String::from_utf8_lossy(&buffer)
        .lines()
        .next()
        .unwrap_or_default()
        .to_string()
}

pub(super) fn slash_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
#[path = "scan_tests.rs"]
mod tests;
