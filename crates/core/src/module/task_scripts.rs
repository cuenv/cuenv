//! Tasks discovered from the script files of a project's `taskScripts`
//! directories.
//!
//! A project opts a directory in from CUE. Each file directly inside it whose
//! interpreter can be told from its first line or its extension becomes a
//! task named after the file, written into the evaluated value next to the
//! declared tasks, so the CLI and the CI compiler read it like any other
//! task. Discovery reads directory entries and each file's first line; it
//! never runs anything.

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::Deserialize;
use serde_json::{Map, Value, json};

/// The project field that opts directories in.
const FIELD: &str = "taskScripts";

/// How much of a file is read to find its shebang.
const FIRST_LINE_LIMIT: u64 = 512;

/// One directory a project opts into discovery.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScriptDirectory {
    /// The directory, relative to the project.
    path: String,
    /// Prepended to each file stem to name its task.
    #[serde(default)]
    prefix: String,
    /// File-name globs that are never tasks.
    #[serde(default)]
    exclude: Vec<String>,
    /// Interpreters chosen by file name, over the shebang and extension.
    #[serde(default)]
    interpreters: BTreeMap<String, String>,
    /// Copied to each task as written; discovered tasks are not hermetic
    /// unless the directory says so.
    #[serde(default)]
    hermetic: Option<Value>,
    /// Extra inputs of each task, after the script itself.
    #[serde(default)]
    inputs: Vec<Value>,
    #[serde(default)]
    outputs: Option<Value>,
    #[serde(default)]
    cache: Option<Value>,
}

/// Where the project being read lies.
#[derive(Debug, Clone, Copy)]
pub(super) struct ScriptPlace<'place> {
    /// The CUE module root.
    pub(super) module_root: &'place Path,
    /// The project's directory relative to the module root, empty for the
    /// root itself.
    pub(super) instance: &'place Path,
}

/// The interpreter a script runs with.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Interpreter {
    /// A supported interpreter, by the program that runs it.
    Program(String),
    /// A shebang naming a program cuenv does not run scripts with.
    Unsupported(String),
    /// A shebang-less file whose extension several interpreters share.
    Ambiguous(String),
}

/// The program for an interpreter name from CUE or a shebang.
fn supported(name: &str) -> Option<String> {
    match name {
        "bash" | "sh" | "fish" | "nu" | "bun" | "python" | "python3" => Some(name.to_string()),
        _ if is_versioned_python(name) => Some(name.to_string()),
        _ => None,
    }
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

/// Choose the interpreter: the CUE override, then the shebang, then an
/// extension only one supported interpreter uses. `None` when the file
/// carries no sign of being a script.
fn interpreter(file_name: &str, first_line: &str, overridden: Option<&str>) -> Option<Interpreter> {
    if let Some(name) = overridden {
        return Some(supported(name).map_or_else(
            || Interpreter::Unsupported(name.to_string()),
            Interpreter::Program,
        ));
    }
    if let Some(program) = shebang_program(first_line) {
        return Some(
            supported(&program).map_or(Interpreter::Unsupported(program), Interpreter::Program),
        );
    }
    let extension = Path::new(file_name).extension()?.to_str()?;
    match extension {
        "bash" => Some(Interpreter::Program("bash".to_string())),
        "fish" => Some(Interpreter::Program("fish".to_string())),
        "nu" => Some(Interpreter::Program("nu".to_string())),
        "py" => Some(Interpreter::Program("python3".to_string())),
        "sh" | "js" | "mjs" | "cjs" | "jsx" | "ts" | "mts" | "cts" | "tsx" => {
            Some(Interpreter::Ambiguous(extension.to_string()))
        }
        _ => None,
    }
}

/// Whether a discovered task name is one a task graph reads as a single
/// task: no dots, which would name a group's child.
fn is_task_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

/// A file found in a script directory.
struct Found {
    name: String,
    /// Relative to the project directory, with `/` separators.
    path: String,
    interpreter: Interpreter,
}

/// Discover the tasks of every directory the project's `taskScripts`
/// declares, write them into its `tasks`, and record each problem against
/// the task it concerns. An instance without `taskScripts` is untouched.
pub(super) fn discover(
    value: &mut Value,
    place: ScriptPlace<'_>,
    problems: &mut BTreeMap<String, Vec<String>>,
) {
    let Some(root) = value.as_object_mut() else {
        return;
    };
    let Some(declared) = root.get(FIELD) else {
        return;
    };
    let directories: Vec<ScriptDirectory> = match serde_json::from_value(declared.clone()) {
        Ok(directories) => directories,
        Err(error) => {
            tracing::warn!(%error, "taskScripts could not be read; no script tasks were discovered");
            return;
        }
    };
    let project = place.module_root.join(place.instance);
    let mut discovered: BTreeMap<String, (Found, Map<String, Value>)> = BTreeMap::new();
    for directory in &directories {
        for found in scan(&project, directory) {
            let task = task_value(&found, directory, place);
            if let Some((first, _)) = discovered.get(&found.name) {
                record(
                    problems,
                    &found.name,
                    format!(
                        "task `{}` is discovered from both `{}` and `{}`; rename one, or exclude one in `taskScripts`",
                        found.name, first.path, found.path
                    ),
                );
                continue;
            }
            discovered.insert(found.name.clone(), (found, task));
        }
    }
    if discovered.is_empty() {
        return;
    }
    let tasks = root
        .entry("tasks")
        .or_insert_with(|| Value::Object(Map::new()));
    let Value::Object(tasks) = tasks else {
        return;
    };
    for (name, (found, task)) in discovered {
        if tasks.contains_key(&name) {
            record(
                problems,
                &name,
                format!(
                    "task `{name}` is declared in CUE and also discovered from `{}`; rename one, or exclude the file in `taskScripts`",
                    found.path
                ),
            );
            continue;
        }
        match &found.interpreter {
            Interpreter::Program(_) => {}
            Interpreter::Unsupported(program) => record(
                problems,
                &name,
                format!(
                    "`{}` runs with `{program}`, which cuenv does not run scripts with; use bash, sh, fish, nu, python or bun, or exclude the file in `taskScripts`",
                    found.path
                ),
            ),
            Interpreter::Ambiguous(extension) => record(
                problems,
                &name,
                format!(
                    "the interpreter of `{}` is ambiguous: a `.{extension}` file needs a shebang such as `#!/usr/bin/env bash`, or an entry in `taskScripts` `interpreters`",
                    found.path
                ),
            ),
        }
        tasks.insert(name, Value::Object(task));
    }
}

fn record(problems: &mut BTreeMap<String, Vec<String>>, name: &str, problem: String) {
    problems.entry(name.to_string()).or_default().push(problem);
}

/// The script files directly inside one directory, sorted by file name.
/// A directory that is missing or leaves the project yields nothing.
fn scan(project: &Path, directory: &ScriptDirectory) -> Vec<Found> {
    let Some((project, location)) = inside(project, &project.join(&directory.path)) else {
        tracing::warn!(
            path = %directory.path,
            "taskScripts directory does not exist inside the project; no script tasks were discovered from it"
        );
        return Vec::new();
    };
    if !location.is_dir() {
        tracing::warn!(path = %directory.path, "taskScripts path is not a directory");
        return Vec::new();
    }
    let excluded = exclusions(&directory.exclude);
    let Ok(entries) = fs::read_dir(&location) else {
        tracing::warn!(path = %directory.path, "taskScripts directory could not be read");
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
            let interpreter = interpreter(
                &file_name,
                &first_line(&target),
                directory.interpreters.get(&file_name).map(String::as_str),
            )?;
            let stem = Path::new(&file_name).file_stem()?.to_str()?;
            let name = format!("{}{stem}", directory.prefix);
            if !is_task_name(&name) {
                tracing::warn!(
                    file = %file_name,
                    "a taskScripts file name is not a task name (letters, digits, `_` and `-`); it was not discovered"
                );
                return None;
            }
            let relative = file.strip_prefix(&project).ok()?;
            Some(Found {
                name,
                path: slash_path(relative),
                interpreter,
            })
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
                tracing::warn!(%pattern, %error, "taskScripts exclude pattern is invalid");
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

fn slash_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// The evaluated value of a discovered task.
fn task_value(
    found: &Found,
    directory: &ScriptDirectory,
    place: ScriptPlace<'_>,
) -> Map<String, Value> {
    let (command, args) = match &found.interpreter {
        Interpreter::Program(program) => (program.clone(), vec![found.path.clone()]),
        // A task that cannot run still needs a command to be read; its
        // problem is recorded and stops it before anything starts.
        Interpreter::Unsupported(_) | Interpreter::Ambiguous(_) => (found.path.clone(), Vec::new()),
    };
    let interpreter = match &found.interpreter {
        Interpreter::Program(program) | Interpreter::Unsupported(program) => program.clone(),
        Interpreter::Ambiguous(_) => String::new(),
    };
    let mut inputs = vec![Value::String(found.path.clone())];
    inputs.extend(directory.inputs.iter().cloned());
    let instance = slash_path(place.instance);
    let source_file = if instance.is_empty() {
        found.path.clone()
    } else {
        format!("{instance}/{}", found.path)
    };
    let mut task = json!({
        "command": command,
        "args": args,
        "inputs": inputs,
        "hermetic": directory.hermetic.clone().unwrap_or(Value::Bool(false)),
        "description": if interpreter.is_empty() {
            format!("Script {}", found.path)
        } else {
            format!("Script {} ({interpreter})", found.path)
        },
        // Scripts run from the project directory, like a task declared there.
        "dir": {"from": "module", "path": if instance.is_empty() { ".".to_string() } else { instance }},
        "_source": {"file": source_file, "line": 1, "column": 1},
        "_cuenvScript": {"path": found.path, "interpreter": interpreter},
    });
    if let Value::Object(task) = &mut task {
        if let Some(outputs) = &directory.outputs {
            task.insert("outputs".to_string(), outputs.clone());
        }
        if let Some(cache) = &directory.cache {
            task.insert("cache".to_string(), cache.clone());
        }
    }
    match task {
        Value::Object(task) => task,
        _ => Map::new(),
    }
}

#[cfg(test)]
#[path = "task_scripts_tests.rs"]
mod tests;
