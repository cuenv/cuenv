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

/// Append what to do to a CUE error that says what is wrong with an input.
///
/// The hint is keyed on the field path in the raw error, never on the
/// wording of the message, which differs between CUE releases: a path of a
/// file reference fails at `…inputs.<n>.paths.<m>: invalid value`, and the
/// raw text of that check is a regular expression.
fn with_input_hints(message: String) -> String {
    if message.lines().any(rejects_a_reference_path) {
        format!("{message}\n{REFERENCE_PATH_HINT}")
    } else {
        message
    }
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
