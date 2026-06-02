use crate::tasks::SourceLocation;
use serde_json::{Map, Value};

use super::SourceMap;

const AUTHORED_TASK_SOURCE_FIELDS: &[&str] = &[
    "command",
    "script",
    "description",
    "scriptShell",
    "shellOptions",
    "hermetic",
    "dir",
    "args",
    "env",
    "dependsOn",
    "inputs",
    "outputs",
    "cache",
    "labels",
    "params",
    "timeout",
    "retry",
    "continueOnError",
    "captures",
    "runtime",
    "dagger",
];

pub(super) struct TaskSourceMaps<'a> {
    pub definitions: &'a SourceMap,
    pub callers: Option<&'a SourceMap>,
}

pub(super) fn enrich_task_sources(
    value: &mut Value,
    instance_path: &str,
    maps: TaskSourceMaps<'_>,
) {
    TaskSourceContext {
        instance_path,
        maps,
    }
    .enrich(value, "");
}

struct TaskSourceContext<'a> {
    instance_path: &'a str,
    maps: TaskSourceMaps<'a>,
}

impl TaskSourceContext<'_> {
    fn enrich(&self, value: &mut Value, field_path: &str) {
        match value {
            Value::Object(obj) => {
                self.enrich_object(obj, field_path);
                self.enrich_children(obj, field_path);
            }
            Value::Array(items) => {
                for (i, child) in items.iter_mut().enumerate() {
                    self.enrich(child, &format!("{field_path}[{i}]"));
                }
            }
            _ => {}
        }
    }

    fn enrich_object(&self, obj: &mut Map<String, Value>, field_path: &str) {
        if !is_executable_task_object(obj) {
            return;
        }

        if !obj.contains_key("_source")
            && let Some(source) = self.definition_source(field_path, obj)
        {
            obj.insert("_source".to_string(), source_value(source));
        }

        if !obj.contains_key("_callerSource")
            && let Some(callers) = self.maps.callers
            && let Some(source) = self.source_for_field(field_path, obj, callers)
        {
            obj.insert("_callerSource".to_string(), source_value(source));
        }
    }

    fn enrich_children(&self, obj: &mut Map<String, Value>, field_path: &str) {
        for (key, child) in obj.iter_mut() {
            if key.starts_with('_') {
                continue;
            }

            let child_path = if field_path.is_empty() {
                key.clone()
            } else {
                format!("{field_path}.{key}")
            };
            self.enrich(child, &child_path);
        }
    }

    fn definition_source(
        &self,
        field_path: &str,
        obj: &Map<String, Value>,
    ) -> Option<&SourceLocation> {
        let exact = self.meta_key(field_path);
        self.executable_body_source(&exact, obj, self.maps.definitions)
            .or_else(|| self.authored_task_field_source(&exact, obj, self.maps.definitions))
            .or_else(|| self.maps.definitions.get(&exact))
            .or_else(|| self.source_for_nearest_ancestor(field_path, self.maps.definitions))
    }

    fn source_for_field<'a>(
        &self,
        field_path: &str,
        obj: &Map<String, Value>,
        sources: &'a SourceMap,
    ) -> Option<&'a SourceLocation> {
        let exact = self.meta_key(field_path);
        sources
            .get(&exact)
            .or_else(|| self.executable_body_source(&exact, obj, sources))
            .or_else(|| self.source_for_nearest_ancestor(field_path, sources))
    }

    fn executable_body_source<'a>(
        &self,
        exact: &str,
        obj: &Map<String, Value>,
        sources: &'a SourceMap,
    ) -> Option<&'a SourceLocation> {
        ["command", "script"]
            .iter()
            .filter(|field| obj.contains_key(**field))
            .find_map(|field| sources.get(&format!("{exact}.{field}")))
    }

    fn authored_task_field_source<'a>(
        &self,
        exact: &str,
        obj: &Map<String, Value>,
        sources: &'a SourceMap,
    ) -> Option<&'a SourceLocation> {
        AUTHORED_TASK_SOURCE_FIELDS
            .iter()
            .filter(|field| obj.contains_key(**field))
            .find_map(|field| self.source_for_task_field(exact, field, sources))
    }

    fn source_for_task_field<'a>(
        &self,
        exact: &str,
        field: &str,
        sources: &'a SourceMap,
    ) -> Option<&'a SourceLocation> {
        let field_key = format!("{exact}.{field}");
        sources
            .get(&field_key)
            .or_else(|| self.source_for_field_descendant(&field_key, sources))
    }

    fn source_for_field_descendant<'a>(
        &self,
        field_key: &str,
        sources: &'a SourceMap,
    ) -> Option<&'a SourceLocation> {
        let mut matches = sources
            .iter()
            .filter(|(key, _)| {
                key.strip_prefix(field_key)
                    .is_some_and(|rest| rest.starts_with('.') || rest.starts_with('['))
            })
            .collect::<Vec<_>>();
        matches.sort_by_key(|(key, _)| *key);
        matches.into_iter().next().map(|(_, source)| source)
    }

    fn source_for_nearest_ancestor<'a>(
        &self,
        field_path: &str,
        sources: &'a SourceMap,
    ) -> Option<&'a SourceLocation> {
        let mut candidate = field_path;
        while let Some((parent, _)) = candidate.rsplit_once(['.', '[']) {
            candidate = parent;
            if let Some(source) = sources.get(&self.meta_key(candidate)) {
                return Some(source);
            }
        }
        None
    }

    fn meta_key(&self, field_path: &str) -> String {
        format!("{}/{}", self.instance_path, field_path)
    }
}

fn is_executable_task_object(obj: &Map<String, Value>) -> bool {
    obj.contains_key("command") || obj.contains_key("script")
}

fn source_value(source: &SourceLocation) -> Value {
    serde_json::to_value(source).expect("source locations should serialize")
}
