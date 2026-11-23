use cuenv_core::ModuleEvaluationMetadata;
use cuenv_core::tasks::SourceLocation;
use std::collections::HashMap;

/// Collect references and source locations from CUE fields across evaluated packages.
#[derive(Default)]
pub struct EvaluationMetadataBuilder {
    references: HashMap<String, String>,
    sources: HashMap<String, SourceLocation>,
    caller_sources: HashMap<String, SourceLocation>,
}

impl EvaluationMetadataBuilder {
    /// Store a field under its module-relative evaluation key.
    pub fn insert(&mut self, key: String, meta: cuengine::FieldMeta) {
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

    /// Produce the metadata used by module evaluation.
    #[must_use]
    pub fn finish(self) -> ModuleEvaluationMetadata {
        ModuleEvaluationMetadata {
            direct_references: HashMap::new(),
            references: non_empty(self.references),
            sources: non_empty(self.sources),
            caller_sources: non_empty(self.caller_sources),
            declarations: None,
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
