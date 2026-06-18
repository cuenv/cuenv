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
            cuenv_core::Error::configuration(message)
        }
        cuengine::CueEngineError::Ffi { function, message } => {
            cuenv_core::Error::ffi(function, message)
        }
        cuengine::CueEngineError::CueParse { path, message } => {
            annotate_cue_parse_error(&path, message)
        }
        cuengine::CueEngineError::Validation { message } => cuenv_core::Error::validation(message),
        cuengine::CueEngineError::Cache { message } => cuenv_core::Error::configuration(message),
    }
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
        return cuenv_core::Error::cue_parse_with_source(path, message, src, None, None);
    };

    let span = byte_offset_of_line(&src, line).map(|offset| {
        // Highlight the entire line (length = chars until newline or end).
        let line_len = src[offset..].find('\n').unwrap_or(src.len() - offset);
        miette::SourceSpan::from((offset, line_len))
    });

    cuenv_core::Error::cue_parse_with_source(path, message, src, span, None)
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
pub struct ModuleGuard<'a> {
    pub(super) guard: MutexGuard<'a, HashMap<PathBuf, ModuleEvaluation>>,
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
