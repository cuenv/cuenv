//! Module utilities for CUE evaluation and error conversion.

use cuenv_core::tasks::SourceLocation;
use cuenv_core::{ModuleEvaluation, ModuleEvaluationMetadata};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::MutexGuard;

#[derive(Default)]
pub(super) struct EvaluationMetadataBuilder {
    references: HashMap<String, String>,
    sources: HashMap<String, SourceLocation>,
    caller_sources: HashMap<String, SourceLocation>,
}

impl EvaluationMetadataBuilder {
    pub(super) fn insert(&mut self, key: String, meta: cuengine::FieldMeta) {
        let source = source_location_from_meta(&meta);
        let caller_source = caller_source_location_from_meta(&meta);

        if let Some(reference) = meta.reference {
            self.references.insert(key.clone(), reference);
        }
        if let Some(source) = source {
            self.sources.insert(key.clone(), source);
        }
        if let Some(caller_source) = caller_source {
            self.caller_sources.insert(key, caller_source);
        }
    }

    pub(super) fn finish(self) -> ModuleEvaluationMetadata {
        ModuleEvaluationMetadata {
            direct_references: HashMap::new(),
            references: non_empty(self.references),
            sources: non_empty(self.sources),
            caller_sources: non_empty(self.caller_sources),
        }
    }
}

fn source_location_from_parts(
    directory: &str,
    filename: &str,
    line: usize,
) -> Option<SourceLocation> {
    if filename.is_empty() {
        return None;
    }

    let file = if filename.contains('/')
        || filename.contains('\\')
        || directory.is_empty()
        || directory == "."
    {
        filename.to_string()
    } else {
        format!("{directory}/{filename}")
    };

    Some(SourceLocation {
        file,
        line: u32::try_from(line).unwrap_or(u32::MAX),
        column: 0,
    })
}

fn source_location_from_meta(meta: &cuengine::FieldMeta) -> Option<SourceLocation> {
    if !meta.definition_filename.is_empty() {
        return source_location_from_parts(
            &meta.definition_directory,
            &meta.definition_filename,
            meta.definition_line,
        );
    }

    source_location_from_parts(&meta.directory, &meta.filename, meta.line)
}

fn caller_source_location_from_meta(meta: &cuengine::FieldMeta) -> Option<SourceLocation> {
    source_location_from_parts(&meta.directory, &meta.filename, meta.line)
}

fn non_empty<K, V>(map: HashMap<K, V>) -> Option<HashMap<K, V>> {
    (!map.is_empty()).then_some(map)
}

/// Compute the byte offset of the start of a 1-based line number in `src`.
///
/// Returns `None` if `line` is 0 or exceeds the number of lines in the source.
fn byte_offset_of_line(src: &str, line: usize) -> Option<usize> {
    if line == 0 {
        return None;
    }
    if line == 1 {
        return Some(0);
    }
    src.char_indices()
        .filter(|&(_, c)| c == '\n')
        .nth(line - 2)
        .map(|(idx, _)| idx + 1)
}

/// Convert cuengine error to `cuenv_core` error.
#[must_use]
pub fn convert_engine_error(err: cuengine::CueEngineError) -> cuenv_core::Error {
    match err {
        cuengine::CueEngineError::Configuration { message } => {
            cuenv_core::Error::configuration(with_input_hints(message))
        }
        cuengine::CueEngineError::Ffi { function, message } => {
            cuenv_core::Error::ffi(function, message)
        }
        cuengine::CueEngineError::CueParse { path, message } => {
            annotate_cue_parse_error(&path, with_input_hints(message))
        }
        other => cuenv_core::Error::configuration(with_input_hints(other.to_string())),
    }
}

/// What to do about a path of a file reference the schema rejected.
const REFERENCE_PATH_HINT: &str = "hint: paths are relative to the project (for \
    `schema.#ModuleFiles`, the module root): no `..`, `.`, `.git` or empty segment, no leading \
    `/`, no surrounding space; for files outside the project use `schema.#ModuleFiles`";

/// What to do about a task used as an input.
const TASK_AS_INPUT_HINT: &str = "hint: a task is not an input: write `<task>.outputFiles` \
    (`content.tasks.build.outputFiles` for a task of another project) to read the files it \
    declares in `outputs`";

/// What to do about a group used as an input.
const GROUP_AS_INPUT_HINT: &str = "hint: groups cannot be inputs yet; list the `outputFiles` \
    of the group's tasks";

/// Append what to do to a CUE error that says what is wrong with an input.
///
/// The hints are keyed on the field path in the raw error, as the CUE
/// evaluator cuenv runs prints it, never on the wording of a message that
/// differs between CUE releases:
///
/// - a path of a file reference fails at `…inputs.<n>.paths.<m>: invalid
///   value`, and the raw text of that check is a regular expression;
/// - a task or group used as an input fails at `…inputs.<n>: conflicting
///   values {…}` with the task's or group's value;
/// - a string input that leaves the project fails at
///   `…._cuenvInputCheck."<input>": …`, after which its path from the module
///   root is given, where the error says which file the project is in.
fn with_input_hints(message: String) -> String {
    let mut hints: Vec<String> = Vec::new();
    if message.lines().any(rejects_a_reference_path) {
        hints.push(REFERENCE_PATH_HINT.to_string());
    }
    match input_node_kind(&message) {
        Some(InputNode::Task) => hints.push(TASK_AS_INPUT_HINT.to_string()),
        Some(InputNode::Group) => hints.push(GROUP_AS_INPUT_HINT.to_string()),
        None => {}
    }
    hints.extend(leaving_inputs(&message));
    if hints.is_empty() {
        message
    } else {
        format!("{message}\n{}", hints.join("\n"))
    }
}

/// What kind of node a CUE error says was used as an input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputNode {
    Task,
    Group,
}

/// The kind of node used as an input, when a line of the error is the
/// schema rejecting one: `<…>.inputs.<n>: conflicting values {<value>} and
/// …`, where the value is a task (it has a `command` or `script`) or a group
/// (`type: "group"`).
fn input_node_kind(message: &str) -> Option<InputNode> {
    message.lines().find_map(|line| {
        let (field, value) = line.split_once(": conflicting values {")?;
        let field = field.split_whitespace().last()?;
        let segments: Vec<&str> = field.split('.').collect();
        let [.., "inputs", index] = segments.as_slice() else {
            return None;
        };
        if index.is_empty() || !index.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        if value.starts_with("type:\"group\"") || value.contains(",type:\"group\"") {
            Some(InputNode::Group)
        } else if value.starts_with("command:")
            || value.starts_with("script:")
            || value.contains(",command:")
            || value.contains(",script:")
        {
            Some(InputNode::Task)
        } else {
            None
        }
    })
}

/// A hint for each string input the schema rejected for leaving its
/// project, giving its path from the module root.
///
/// The project's directory is the directory of the first position of the
/// error in a file of the consuming module: the schema's own files (its
/// `schema/` directory in this repository, an absolute path in the module
/// cache) and `cue.mod` are skipped.
fn leaving_inputs(message: &str) -> Vec<String> {
    let lines: Vec<&str> = message.lines().collect();
    let mut hints = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let Some((_, rest)) = line.split_once("._cuenvInputCheck.\"") else {
            continue;
        };
        let Some((input, _)) = rest.split_once("\":") else {
            continue;
        };
        let directory = lines[index + 1..]
            .iter()
            .map(|position| position.trim())
            .take_while(|position| position.starts_with("./") || position.starts_with('/'))
            .filter_map(|position| position.strip_prefix("./"))
            .filter(|position| {
                !position.starts_with("schema/") && !position.starts_with("cue.mod/")
            })
            .find_map(|position| {
                let file = position.split(':').next()?;
                Some(
                    Path::new(file)
                        .parent()
                        .map(Path::to_path_buf)
                        .unwrap_or_default(),
                )
            });
        let Some(directory) = directory else {
            continue;
        };
        let mut resolved = PathBuf::new();
        let mut inside = true;
        for component in directory.join(input).components() {
            match component {
                std::path::Component::Normal(part) => resolved.push(part),
                std::path::Component::ParentDir => inside &= resolved.pop(),
                std::path::Component::CurDir => {}
                std::path::Component::RootDir | std::path::Component::Prefix(_) => inside = false,
            }
        }
        let hint = if inside && !resolved.as_os_str().is_empty() {
            format!(
                "hint: `{input}` is `{}` from the module root: write \
                 `schema.#ModuleFiles & {{paths: [\"{}\"]}}`, or import the project that holds \
                 it and write `<pkg>.#Files & {{paths: [...]}}` with paths from that project's \
                 directory",
                resolved.to_string_lossy().replace('\\', "/"),
                resolved.to_string_lossy().replace('\\', "/")
            )
        } else {
            format!(
                "hint: `{input}` leaves the CUE module, and nothing above the module root can be \
                 named"
            )
        };
        if !hints.contains(&hint) {
            hints.push(hint);
        }
    }
    hints
}

/// Whether a line of a CUE error is the schema rejecting one path of a file
/// reference: `<…>.inputs.<n>.paths.<m>: invalid value …`.
fn rejects_a_reference_path(line: &str) -> bool {
    let Some((field, _)) = line.split_once(": invalid value") else {
        return false;
    };
    let Some(field) = field.split_whitespace().last() else {
        return false;
    };
    let segments: Vec<&str> = field.split('.').collect();
    let is_index =
        |segment: &str| !segment.is_empty() && segment.bytes().all(|b| b.is_ascii_digit());
    matches!(
        segments.as_slice(),
        [.., "inputs", input, "paths", path] if is_index(input) && is_index(path)
    )
}

/// Build a `CueParse` error, populating source context when possible.
///
/// Reads the file at `path`, parses the `LINE:COL` from the Go error message,
/// and computes a `miette::SourceSpan` pointing at that line. Falls back to
/// a plain error without source context if anything fails.
fn annotate_cue_parse_error(path: &std::path::Path, message: String) -> cuenv_core::Error {
    let Some(src) = std::fs::read_to_string(path).ok() else {
        return cuenv_core::Error::cue_parse(path, message);
    };

    let Some((line, _col)) = parse_line_col_from_message(&message) else {
        return cuenv_core::Error::cue_parse_with_source(
            path,
            message,
            cuenv_core::error::CueParseSource {
                text: src,
                span: None,
                suggestions: None,
            },
        );
    };

    let span = byte_offset_of_line(&src, line).map(|offset| {
        // Highlight the entire line (length = chars until newline or end).
        let line_len = src[offset..].find('\n').unwrap_or(src.len() - offset);
        miette::SourceSpan::from((offset, line_len))
    });

    cuenv_core::Error::cue_parse_with_source(
        path,
        message,
        cuenv_core::error::CueParseSource {
            text: src,
            span,
            suggestions: None,
        },
    )
}

/// Extract the first `LINE:COL` pair from a CUE error message.
///
/// CUE errors from the Go bridge use the format:
/// `./env.cue:LINE:COL: description` or `path/file.cue:LINE:COL: description`.
///
/// Iterates over `:` splits and returns the first pair of consecutive
/// positive integers, skipping any leading path component.
fn parse_line_col_from_message(message: &str) -> Option<(usize, usize)> {
    let parts: Vec<&str> = message.split(':').collect();
    for i in 0..parts.len().saturating_sub(1) {
        let maybe_line = parts[i].trim().parse::<usize>();
        let maybe_col = parts[i + 1].trim().parse::<usize>();
        if let (Ok(line), Ok(col)) = (maybe_line, maybe_col)
            && line > 0
        {
            return Some((line, col));
        }
    }
    None
}

/// Compute the relative path from module root to target directory.
///
/// Returns the path suitable for looking up instances in `ModuleEvaluation`.
/// Returns `"."` for the module root itself.
#[must_use]
pub fn relative_path_from_root(module_root: &Path, target: &Path) -> PathBuf {
    target.strip_prefix(module_root).map_or_else(
        |_| PathBuf::from("."),
        |p| {
            if p.as_os_str().is_empty() {
                PathBuf::from(".")
            } else {
                p.to_path_buf()
            }
        },
    )
}

/// A guard that provides access to the loaded `ModuleEvaluation`.
///
/// This wrapper around `MutexGuard` holds a `HashMap<PathBuf, ModuleEvaluation>`
/// and a lookup key. The keyed entry is guaranteed to exist in the map while
/// the guard is held.
pub struct ModuleGuard<'guard> {
    pub(super) guard: MutexGuard<'guard, HashMap<PathBuf, ModuleEvaluation>>,
    pub(super) key: PathBuf,
}

impl std::ops::Deref for ModuleGuard<'_> {
    type Target = ModuleEvaluation;

    fn deref(&self) -> &Self::Target {
        self.guard.get(&self.key).unwrap_or_else(|| {
            unreachable!("ModuleGuard invariant violated: module should be loaded")
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rejected_reference_path_gets_the_rules_appended() {
        // The raw CUE text of the schema's path check, before cuenv wraps it.
        let raw = "tasks.build: 4 errors in empty disjunction:\n    \
                   tasks.build.inputs.1.paths.0: invalid value \"../shared/**\" (out of bound \
                   !~\"(^|/)\\\\.\\\\.?(/|$)\"):\n        ./schema/tasks.cue:330:50";

        let hinted = with_input_hints(raw.to_string());

        assert!(hinted.starts_with(raw));
        assert!(hinted.ends_with(REFERENCE_PATH_HINT), "{hinted}");
    }

    #[test]
    fn other_invalid_values_get_no_reference_hint() {
        for raw in [
            "tasks.build.args.0: invalid value \"x\"",
            "tasks.build.inputs.0: invalid value \"x\"",
            "tasks.build.inputs.paths.0: invalid value \"x\"",
            "tasks.build.inputs.0.paths: incompatible list lengths (0 and 1)",
            "ci.pipelines.default.paths.0: invalid value \"x\"",
        ] {
            assert_eq!(with_input_hints(raw.to_string()), raw, "{raw}");
        }
    }

    #[test]
    fn a_task_or_a_group_used_as_an_input_gets_its_hint() {
        // The raw lines of the evaluator cuenv runs, as it reports a task
        // and a group used as an input.
        let task = "tasks.build: 8 errors in empty disjunction:\n  \
                    tasks.build.inputs.1: conflicting values {command:\"sh\",args:[\"-c\",\"x\"],\
                    inputs:[\"articles/**\"],outputs:[\"dist/built.txt\"]} and string \
                    (mismatched types struct and string):\n      ./app/env.cue:8:1";
        let group = "tasks.build.inputs.1: conflicting values {type:\"group\",lint:\
                     {command:\"true\",inputs:[\"articles/**\"]}} and string (mismatched types \
                     struct and string):";

        assert!(with_input_hints(task.to_string()).ends_with(TASK_AS_INPUT_HINT));
        assert!(with_input_hints(group.to_string()).ends_with(GROUP_AS_INPUT_HINT));
        // A struct that is no task is left alone.
        let other = "tasks.build.inputs.1: conflicting values {paths:[\"a\"]} and string";
        assert_eq!(with_input_hints(other.to_string()), other);
    }

    #[test]
    fn an_input_that_leaves_the_project_gets_its_path_from_the_module_root() {
        let raw = "tasks.build._cuenvInputCheck.\"../content/articles/**\": an input cannot \
                   leave the project directory:\n    ./schema/tasks.cue:225:22\n    \
                   ./app/env.cue:8:1\n    ./app/env.cue:16:21";

        let hinted = with_input_hints(raw.to_string());

        assert!(
            hinted.ends_with(
                "hint: `../content/articles/**` is `content/articles/**` from the module root: \
                 write `schema.#ModuleFiles & {paths: [\"content/articles/**\"]}`, or import the \
                 project that holds it and write `<pkg>.#Files & {paths: [...]}` with paths from \
                 that project's directory"
            ),
            "{hinted}"
        );
        let outside = raw.replace("../content", "../../outside");
        assert!(
            with_input_hints(outside).contains("leaves the CUE module"),
            "{hinted}"
        );
    }

    #[test]
    fn an_engine_error_carries_the_hint() {
        let error = convert_engine_error(cuengine::CueEngineError::configuration(
            "tasks.a.inputs.2.paths.1: invalid value \"/etc\"",
        ));
        assert!(
            error.to_string().contains("for files outside the project"),
            "{error}"
        );
    }
}
