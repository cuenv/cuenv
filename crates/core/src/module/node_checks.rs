//! Checks of a project's evaluated value that belong to single nodes.
//!
//! The schema stamps every task, group, service and image with the project
//! it belongs to (`cuenvRef: {kind, project, name}`), and every output
//! reference with its task's project (`cuenvProject`). A value that came
//! from another project through an import therefore says so. Until
//! cross-project task references are bound through imports, such a value
//! is never named by its bare name, which would bind a same-named node of
//! this project. It becomes a problem of the node that holds it instead:
//!
//! - a `dependsOn` entry of another project (nested sequence lists
//!   inspected, never an embedded dependency's own `dependsOn`);
//! - another project's task, group, service or image in an output
//!   reference, a CI pipeline's `tasks`, a matrix task's `task`, a
//!   service's `watch.rebuild`, or a service's or image's `dependsOn`.
//!
//! The entry is removed and the problem recorded against the node (see
//! [`NodeDiagnostics`]), which fails only what reaches that node.
//!
//! The evaluation is lenient: a value `cue export` rejects can still
//! arrive. A file reference without paths (`inputs: [content.#Files]`), a
//! `null` input and the like would fail the whole project when it is
//! deserialized; they are removed and recorded against their task here, so
//! the rest of the project loads.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::manifest::NodeDiagnostics;
use crate::tasks::SourceLocation;
use crate::tasks::file_references::for_task;

/// The key a project's node diagnostics are written under, which
/// [`crate::manifest::Project`] reads them from.
pub(super) const DIAGNOSTICS_FIELD: &str = "_cuenvDiagnostics";

/// Fields of a group that are not children.
const GROUP_FIELDS: &[&str] = &[
    "type",
    "dependsOn",
    "maxConcurrency",
    "description",
    "cuenvRef",
    "_name",
];

/// What replaces an entry that belongs to another project until it is
/// removed: replacing keeps the indices reference metadata is keyed on.
const FOREIGN_MARKER: &str = "_cuenvForeign";

/// Check the identities in a project instance's value, before reference
/// metadata names any dependency: the stamps of whole sequences are still
/// there to inspect. Every entry of another project is replaced by a
/// marker, which [`check_inputs`] removes once names are assigned. An
/// instance without a `name` is no project and is left alone.
pub(super) fn check_identities(value: &mut Value) -> NodeDiagnostics {
    let Some(root) = value.as_object_mut() else {
        return NodeDiagnostics::default();
    };
    let Some(owner) = root
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|name| !name.is_empty())
    else {
        return NodeDiagnostics::default();
    };
    let mut checks = NodeChecks {
        owner: &owner,
        diagnostics: NodeDiagnostics::default(),
    };
    if let Some(Value::Object(tasks)) = root.get_mut("tasks") {
        for (name, node) in tasks.iter_mut() {
            checks.task_node(name, node);
        }
    }
    if let Some(Value::Object(services)) = root.get_mut("services") {
        for (name, service) in services.iter_mut() {
            let problems = worded("service", name, checks.service(service));
            record(&mut checks.diagnostics.services, name, problems);
        }
    }
    if let Some(Value::Object(images)) = root.get_mut("images") {
        for (name, image) in images.iter_mut() {
            let problems = worded("image", name, checks.image(image));
            record(&mut checks.diagnostics.images, name, problems);
        }
    }
    if let Some(Value::Object(pipelines)) =
        root.get_mut("ci").and_then(|ci| ci.get_mut("pipelines"))
    {
        for (name, pipeline) in pipelines.iter_mut() {
            let problems = checks.pipeline(name, pipeline);
            record(&mut checks.diagnostics.pipelines, name, problems);
        }
    }
    checks.diagnostics
}

/// Remove what [`check_identities`] marked, check every task's inputs, and
/// write the problems found by both into the value, where the project reads
/// them. Runs once tasks carry their source locations.
pub(super) fn check_inputs(value: &mut Value, mut diagnostics: NodeDiagnostics) {
    remove_marked(value);
    let Some(root) = value.as_object_mut() else {
        return;
    };
    let is_project = root
        .get("name")
        .and_then(Value::as_str)
        .is_some_and(|name| !name.is_empty());
    if !is_project {
        return;
    }
    if let Some(Value::Object(tasks)) = root.get_mut("tasks") {
        for (name, node) in tasks.iter_mut() {
            task_inputs(name, node, &mut diagnostics.tasks);
        }
    }
    if !diagnostics.is_empty()
        && let Ok(diagnostics) = serde_json::to_value(&diagnostics)
    {
        root.insert(DIAGNOSTICS_FIELD.to_string(), diagnostics);
    }
}

/// Remove every marked entry from every list of `value`.
fn remove_marked(value: &mut Value) {
    match value {
        Value::Array(items) => {
            items.retain(|item| item.get(FOREIGN_MARKER).is_none());
            for item in items {
                remove_marked(item);
            }
        }
        Value::Object(object) => {
            for nested in object.values_mut() {
                remove_marked(nested);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

/// Check the inputs of the task, group or sequence called `name` and of
/// everything below it.
fn task_inputs(name: &str, node: &mut Value, into: &mut BTreeMap<String, Vec<String>>) {
    match node {
        Value::Array(steps) => {
            for (index, step) in steps.iter_mut().enumerate() {
                task_inputs(&format!("{name}[{index}]"), step, into);
            }
        }
        Value::Object(object) if is_group(object) => {
            for (child, child_node) in object.iter_mut() {
                if !GROUP_FIELDS.contains(&child.as_str()) {
                    task_inputs(&format!("{name}.{child}"), child_node, into);
                }
            }
        }
        Value::Object(object) => {
            let source = source_of(object);
            let problems: Vec<String> = unreadable_inputs(object)
                .iter()
                .map(|problem| for_task(name, source.as_ref(), problem))
                .collect();
            record(into, name, problems);
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn marker() -> Value {
    serde_json::json!({ FOREIGN_MARKER: true })
}

/// Problems of the `kind` called `name`, each saying which node it is.
fn worded(kind: &str, name: &str, problems: Vec<String>) -> Vec<String> {
    problems
        .into_iter()
        .map(|problem| format!("{kind} '{name}': {problem}"))
        .collect()
}

fn record(into: &mut BTreeMap<String, Vec<String>>, name: &str, problems: Vec<String>) {
    if !problems.is_empty() {
        into.entry(name.to_string()).or_default().extend(problems);
    }
}

/// What a stamp says a value is: `task 'build' of project 'content'`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Identity {
    kind: String,
    project: String,
    name: String,
}

impl std::fmt::Display for Identity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = if self.kind.is_empty() {
            "node"
        } else {
            &self.kind
        };
        if self.name.is_empty() {
            write!(formatter, "a {kind} of project '{}'", self.project)
        } else {
            write!(
                formatter,
                "{kind} '{}' of project '{}'",
                self.name, self.project
            )
        }
    }
}

/// The identity the stamp of `value` gives, when it names a project.
fn stamped(value: &Value) -> Option<Identity> {
    let stamp = value.get("cuenvRef")?;
    let text = |field: &str| {
        stamp
            .get(field)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let project = text("project");
    (!project.is_empty()).then(|| Identity {
        kind: text("kind"),
        project,
        name: text("name"),
    })
}

struct NodeChecks<'owner> {
    owner: &'owner str,
    diagnostics: NodeDiagnostics,
}

impl NodeChecks<'_> {
    /// The identity of `value` when it belongs to another project. A
    /// sequence belongs to another project when any member does.
    fn foreign(&self, value: &Value) -> Option<Identity> {
        match value {
            Value::Array(members) => members.iter().find_map(|member| self.foreign(member)),
            Value::Object(_) => stamped(value).filter(|identity| identity.project != self.owner),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => None,
        }
    }

    /// Check a task, group or sequence called `name` and everything below
    /// it.
    fn task_node(&mut self, name: &str, node: &mut Value) {
        match node {
            Value::Array(steps) => {
                for (index, step) in steps.iter_mut().enumerate() {
                    self.task_node(&format!("{name}[{index}]"), step);
                }
            }
            Value::Object(object) => {
                let mut problems = self.task_dependencies(object);
                if is_group(object) {
                    for (child, child_node) in object.iter_mut() {
                        if !GROUP_FIELDS.contains(&child.as_str()) {
                            self.task_node(&format!("{name}.{child}"), child_node);
                        }
                    }
                } else {
                    problems.extend(self.output_references(object));
                }
                let worded = problems
                    .iter()
                    .map(|problem| for_task(name, None, problem))
                    .collect();
                record(&mut self.diagnostics.tasks, name, worded);
            }
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }

    /// Remove the entries of a task's or group's `dependsOn` that belong to
    /// another project, and say why (the interim rule).
    fn task_dependencies(&self, object: &mut Map<String, Value>) -> Vec<String> {
        self.foreign_entries(object, "dependsOn")
            .into_iter()
            .map(|(index, identity)| {
                format!(
                    "dependsOn[{index}] is {identity}: tasks of another project cannot be \
                     depended on through an import yet, and are never bound by name to a task of \
                     this project. Cross-project task references arrive in a later cuenv \
                     release; to read that project's files, use `{}.#Files & {{paths: \
                     [...]}}`",
                    identity.project
                )
            })
            .collect()
    }

    /// Mark the entries of the list `field` of `object` that belong to
    /// another project for removal, returning each with its index.
    fn foreign_entries(
        &self,
        object: &mut Map<String, Value>,
        field: &str,
    ) -> Vec<(usize, Identity)> {
        let Some(Value::Array(entries)) = object.get_mut(field) else {
            return Vec::new();
        };
        let mut found = Vec::new();
        for (index, entry) in entries.iter_mut().enumerate() {
            if let Some(identity) = self.foreign(entry) {
                found.push((index, identity));
                *entry = marker();
            }
        }
        found
    }

    /// Problems with output references to another project's tasks or images
    /// anywhere in a node, other than in its own computed references and in
    /// the nodes it embeds.
    fn output_references(&self, object: &Map<String, Value>) -> Vec<String> {
        let mut problems = Vec::new();
        for (field, value) in object {
            if matches!(
                field.as_str(),
                "stdout"
                    | "stderr"
                    | "exitCode"
                    | "ref"
                    | "digest"
                    | "dependsOn"
                    | "inputs"
                    | "watch"
                    | "cuenvRef"
            ) {
                continue;
            }
            self.collect_output_references(value, &mut problems);
        }
        problems
    }

    fn collect_output_references(&self, value: &Value, problems: &mut Vec<String>) {
        match value {
            Value::Object(object) if is_output_reference(object) => {
                let project = object
                    .get("cuenvProject")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !project.is_empty() && project != self.owner {
                    let target = object
                        .get("cuenvTask")
                        .or_else(|| object.get("cuenvImage"))
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let output = object
                        .get("cuenvOutput")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    problems.push(format!(
                        "the output reference `{target}.{output}` of project '{project}' is not \
                         supported across projects yet: pass files through that task's outputs \
                         and order tasks through dependsOn"
                    ));
                }
            }
            Value::Object(object) => {
                for nested in object.values() {
                    self.collect_output_references(nested, problems);
                }
            }
            Value::Array(items) => {
                for item in items {
                    self.collect_output_references(item, problems);
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }

    fn service(&self, service: &mut Value) -> Vec<String> {
        let Value::Object(object) = service else {
            return Vec::new();
        };
        let mut problems = self.unsupported(object, "dependsOn");
        if let Some(Value::Object(watch)) = object.get_mut("watch") {
            problems.extend(self.unsupported(watch, "rebuild"));
        }
        problems.extend(self.output_references(object));
        problems
    }

    fn image(&self, image: &mut Value) -> Vec<String> {
        let Value::Object(object) = image else {
            return Vec::new();
        };
        let mut problems = self.unsupported(object, "dependsOn");
        problems.extend(self.output_references(object));
        problems
    }

    fn pipeline(&self, name: &str, pipeline: &mut Value) -> Vec<String> {
        let Some(Value::Array(entries)) = pipeline.get_mut("tasks") else {
            return Vec::new();
        };
        let mut problems = Vec::new();
        for (index, entry) in entries.iter_mut().enumerate() {
            // A matrix entry names its task in `task`.
            let named = entry
                .as_object()
                .filter(|object| !object.contains_key("cuenvRef"))
                .and_then(|object| object.get("task"))
                .unwrap_or(entry);
            if let Some(identity) = self.foreign(named) {
                problems.push(format!(
                    "pipeline '{name}' entry tasks[{index}] is {identity}: not supported across \
                     projects yet; run it from a pipeline of project '{}'",
                    identity.project
                ));
                *entry = marker();
            }
        }
        problems
    }

    /// Remove the entries of `field` that belong to another project, where
    /// no cross-project reference is supported yet (R6).
    fn unsupported(&self, object: &mut Map<String, Value>, field: &str) -> Vec<String> {
        self.foreign_entries(object, field)
            .into_iter()
            .map(|(index, identity)| {
                format!("{field}[{index}] is {identity}: not supported across projects yet")
            })
            .collect()
    }
}

fn is_group(object: &Map<String, Value>) -> bool {
    object.get("type").and_then(Value::as_str) == Some("group")
}

fn is_output_reference(object: &Map<String, Value>) -> bool {
    object.get("cuenvOutputRef").and_then(Value::as_bool) == Some(true)
}

fn source_of(object: &Map<String, Value>) -> Option<SourceLocation> {
    object
        .get("_source")
        .and_then(|source| serde_json::from_value(source.clone()).ok())
}

/// Remove the inputs of a task that cannot be read, saying why each one
/// was removed.
fn unreadable_inputs(object: &mut Map<String, Value>) -> Vec<String> {
    let Some(inputs) = object.get_mut("inputs") else {
        return Vec::new();
    };
    let Value::Array(entries) = inputs else {
        let problem = "`inputs` is not a list".to_string();
        object.remove("inputs");
        return vec![problem];
    };
    let mut problems = Vec::new();
    let mut index = 0;
    entries.retain(|entry| {
        let problem = input_problem(entry);
        if let Some(problem) = &problem {
            problems.push(format!("inputs[{index}] {problem}"));
        }
        index += 1;
        problem.is_none()
    });
    problems
}

/// Why one input cannot be read, if it cannot.
fn input_problem(entry: &Value) -> Option<String> {
    match entry {
        Value::Null => Some("has no value".to_string()),
        Value::Object(object) if object.contains_key("cuenvFiles") => {
            file_reference_problem(object)
        }
        Value::Object(_)
        | Value::Bool(_)
        | Value::Number(_)
        | Value::String(_)
        | Value::Array(_) => None,
    }
}

/// Why a file reference as exported cannot be read, if it cannot.
fn file_reference_problem(object: &Map<String, Value>) -> Option<String> {
    let kind = object.get("cuenvFiles").and_then(Value::as_str);
    let project = object.get("cuenvProject");
    let spelling = match (kind, project.and_then(Value::as_str)) {
        (Some("project"), Some(name)) if !name.is_empty() => format!("{name}.#Files"),
        (Some("project"), _) => {
            return Some(
                "is a file reference that names no project: write it through the project's \
                 import, `<pkg>.#Files & {paths: [...]}`"
                    .to_string(),
            );
        }
        (Some("module"), None) if project.is_none() => "schema.#ModuleFiles".to_string(),
        (Some("module"), _) => {
            return Some(
                "is a module file reference that names a project: `schema.#ModuleFiles` paths \
                 start from the module root"
                    .to_string(),
            );
        }
        (Some(_) | None, _) => {
            return Some(
                "is not a file reference cuenv knows: write `<pkg>.#Files & {paths: [...]}` or \
                 `schema.#ModuleFiles & {paths: [...]}`"
                    .to_string(),
            );
        }
    };
    let say_which = || format!("names no files: say which, `{spelling} & {{paths: [...]}}`");
    match object.get("paths") {
        Some(Value::Array(paths)) if paths.is_empty() => Some(say_which()),
        Some(Value::Array(paths)) if paths.iter().all(Value::is_string) => None,
        Some(Value::Array(_)) => Some(format!(
            "has a path that is not a string: `{spelling} & {{paths: [\"...\"]}}`"
        )),
        Some(_) | None => Some(say_which()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Project;
    use serde_json::json;

    fn task(project: &str, name: &str) -> Value {
        json!({
            "cuenvRef": {"kind": "task", "project": project, "name": name},
            "command": "true",
        })
    }

    fn check_nodes(value: &mut Value) {
        let diagnostics = check_identities(value);
        check_inputs(value, diagnostics);
    }

    fn checked(mut value: Value) -> Project {
        check_nodes(&mut value);
        serde_json::from_value(value).expect("the project loads")
    }

    #[test]
    fn a_dependency_on_another_projects_task_is_never_bound_by_name() {
        let mut consumer = task("app", "deploy");
        consumer["dependsOn"] = json!([
            {"_name": "build", "cuenvRef": {"kind": "task", "project": "content", "name": "build"}, "command": "true"},
            {"_name": "lint", "cuenvRef": {"kind": "task", "project": "app", "name": "lint"}, "command": "true"},
        ]);
        let project = checked(json!({
            "name": "app",
            "tasks": {"build": task("app", "build"), "lint": task("app", "lint"), "deploy": consumer},
        }));

        let deploy = project.tasks["deploy"].as_task().expect("a task");
        let names: Vec<&str> = deploy.dependency_names().collect();
        assert_eq!(names, ["lint"], "the local build is not bound");
        let problems = project.diagnostics.task("deploy");
        assert_eq!(problems.len(), 1);
        assert!(
            problems[0].contains("dependsOn[0] is task 'build' of project 'content'"),
            "{problems:?}"
        );
        assert!(problems[0].contains("content.#Files"), "{problems:?}");
        assert!(project.diagnostics.task("build").is_empty());
    }

    #[test]
    fn a_dependency_on_a_sequence_is_inspected_member_by_member() {
        let mut group = json!({
            "type": "group",
            "cuenvRef": {"kind": "group", "project": "app", "name": "checks"},
            "dependsOn": [[task("app", "a"), [task("content", "b")]]],
            "unit": task("app", "checks.unit"),
        });
        group["unit"]["dependsOn"] = json!([task("content", "build")]);
        let project = checked(json!({"name": "app", "tasks": {"checks": group}}));

        assert_eq!(project.diagnostics.task("checks").len(), 1);
        assert_eq!(project.diagnostics.task("checks.unit").len(), 1);
        let checks = project.tasks["checks"].as_group().expect("a group");
        assert!(checks.depends_on.is_empty());
    }

    #[test]
    fn another_projects_pipeline_entry_is_unsupported() {
        let project = checked(json!({
            "name": "app",
            "tasks": {"build": task("app", "build")},
            "ci": {"pipelines": {
                "default": {"tasks": [
                    {"_name": "build", "cuenvRef": {"kind": "task", "project": "content", "name": "build"}, "command": "true"},
                    {"_name": "build", "cuenvRef": {"kind": "task", "project": "app", "name": "build"}, "command": "true"},
                ]},
                "release": {"tasks": [
                    {"type": "matrix", "matrix": {"arch": ["x86"]}, "task": task("content", "pack")},
                ]},
            }},
        }));

        let default = project.diagnostics.pipeline("default");
        assert_eq!(default.len(), 1);
        assert!(
            default[0].contains("tasks[0] is task 'build' of project 'content'"),
            "{default:?}"
        );
        assert!(default[0].contains("not supported across projects yet"));
        assert_eq!(project.diagnostics.pipeline("release").len(), 1);
        let ci = project.ci.expect("ci");
        assert_eq!(ci.pipelines["default"].tasks.len(), 1);
        assert!(ci.pipelines["release"].tasks.is_empty());
    }

    #[test]
    fn another_projects_output_reference_is_unsupported() {
        let mut consumer = task("app", "deploy");
        consumer["args"] = json!([
            {"cuenvOutputRef": true, "cuenvProject": "content", "cuenvTask": "build", "cuenvOutput": "stdout"},
            {"cuenvOutputRef": true, "cuenvProject": "app", "cuenvTask": "lint", "cuenvOutput": "stdout"},
        ]);
        consumer["stdout"] = json!(
            {"cuenvOutputRef": true, "cuenvProject": "app", "cuenvTask": "deploy", "cuenvOutput": "stdout"}
        );
        let mut value = json!({"name": "app", "tasks": {"deploy": consumer}});
        check_nodes(&mut value);

        let problems = &value[DIAGNOSTICS_FIELD]["tasks"]["deploy"];
        assert_eq!(problems.as_array().map(Vec::len), Some(1), "{problems}");
        assert!(
            problems[0]
                .as_str()
                .is_some_and(|problem| problem.contains("`build.stdout` of project 'content'"))
        );
    }

    #[test]
    fn service_and_image_dependencies_of_another_project_are_unsupported() {
        let mut value = json!({
            "name": "app",
            "services": {"db": {
                "cuenvRef": {"kind": "service", "project": "app", "name": "db"},
                "command": "postgres",
                "dependsOn": [task("content", "build"), task("app", "migrate")],
                "watch": {"paths": ["src/**"], "rebuild": [task("content", "gen")]},
            }},
            "images": {"api": {
                "cuenvRef": {"kind": "image", "project": "app", "name": "api"},
                "type": "image",
                "context": ".",
                "dependsOn": [{"cuenvRef": {"kind": "image", "project": "content", "name": "base"}}],
            }},
        });
        check_nodes(&mut value);

        let diagnostics: NodeDiagnostics =
            serde_json::from_value(value[DIAGNOSTICS_FIELD].clone()).expect("diagnostics");
        assert_eq!(diagnostics.service("db").len(), 2, "{diagnostics:?}");
        assert!(diagnostics.service("db")[1].contains("rebuild[0] is task 'gen'"));
        assert_eq!(diagnostics.image("api").len(), 1, "{diagnostics:?}");
        assert_eq!(
            value["services"]["db"]["dependsOn"]
                .as_array()
                .map(Vec::len),
            Some(1),
            "the local dependency remains"
        );
    }

    #[test]
    fn a_malformed_file_reference_fails_its_task_and_the_rest_loads() {
        let mut broken = task("app", "build");
        broken["_source"] = json!({"file": "app/env.cue", "line": 9, "column": 2});
        broken["inputs"] = json!([
            "src/**",
            {"cuenvFiles": "project", "cuenvProject": "content", "paths": null},
            null,
            {"cuenvFiles": "project", "cuenvProject": "", "paths": ["a"]},
            {"cuenvFiles": "module", "paths": []},
            {"cuenvFiles": "module", "cuenvProject": "content", "paths": ["a"]},
            {"cuenvFiles": "module", "paths": [1]},
            {"cuenvFiles": "elsewhere", "paths": ["a"]},
            {"cuenvFiles": "module", "paths": ["bun.lock"]},
        ]);
        let project = checked(json!({
            "name": "app",
            "tasks": {"build": broken, "test": task("app", "test")},
        }));

        let problems = project.diagnostics.task("build");
        assert_eq!(problems.len(), 7, "{problems:?}");
        assert!(
            problems[0].starts_with("task 'build' (app/env.cue:9): inputs[1] names no files"),
            "{problems:?}"
        );
        assert!(problems[0].contains("content.#Files & {paths: [...]}"));
        let build = project.tasks["build"].as_task().expect("a task");
        assert_eq!(build.inputs.len(), 2, "the readable inputs remain");
        assert!(project.diagnostics.task("test").is_empty());
    }

    #[test]
    fn a_value_without_a_name_is_no_project_and_is_left_alone() {
        let mut value = json!({"tasks": {"build": task("content", "build")}});
        let before = value.clone();
        check_nodes(&mut value);
        assert_eq!(value, before);
    }
}
