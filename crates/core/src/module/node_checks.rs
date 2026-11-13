//! Checks of a project's evaluated value that belong to single nodes, and
//! the names its references bind to.
//!
//! The schema stamps every task, group, service and image with the project
//! it belongs to (`cuenvRef: {kind, project, name}`), every output
//! reference with its task's project (`cuenvProject`), and every task's
//! `outputFiles` with its project and name. A value that came from another
//! project through an import therefore says so, and a stamp that names a
//! project is authoritative:
//!
//! - a task's or group's `dependsOn` entry of this project is named by its
//!   stamp, and one of another project `#project:name` (a whole sequence of
//!   another project, a step of one and another project's image or service
//!   cannot be named and are problems of the node instead); a stamp that
//!   names no project (a task outside every project) is named by reference
//!   metadata, and is a problem when that metadata names nothing;
//! - a task's `outputFiles` among another task's inputs reads as the
//!   task-output input `{task: "<name>"}`, or `{task: "#project:name"}`;
//! - another project's task, group, service or image in an output
//!   reference, a CI pipeline's `tasks`, a matrix task's `task`, a
//!   service's `watch.rebuild`, or a service's or image's `dependsOn` is not
//!   supported across projects yet.
//!
//! A stamp is this project's when it names this project and, where the
//! evaluation records where values are declared, its project is declared
//! where this instance's `name` is. A project of another package with the
//! same name (one vendored from outside the module, say) is another project:
//! its task is `#name:task`, which binds among the imports like any other
//! and fails there, never a task of this project.
//!
//! An entry that cannot be read is removed and the problem recorded against
//! the node (see [`NodeDiagnostics`]), which fails only what reaches that
//! node.
//!
//! The evaluation is lenient: a value `cue export` rejects can still
//! arrive, and so can the shapes of an older pinned schema. A string input
//! that leaves the project (`../content/**`), the removed
//! `{project, task, map}` reference and `#project:task:path` shorthand, a
//! file reference without paths (`inputs: [content.#Files]`), a `null`
//! input and the like would fail the whole project when it is deserialized,
//! or worse be read as something else; they are removed and recorded
//! against their task here, so the rest of the project loads.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use serde_json::{Map, Value};

use super::SourceMap;
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

/// Where the values of one instance are declared: what tells this
/// project's own stamps from those of a same-named project.
#[derive(Debug, Clone, Copy)]
pub(super) struct Declarations<'declarations> {
    /// The instance's key in the evaluation, which field paths are
    /// prefixed with.
    pub(super) instance: &'declarations str,
    /// Where each value is declared, by `<instance>/<field path>`; `None`
    /// when the evaluation recorded nothing.
    pub(super) at: Option<&'declarations SourceMap>,
}

impl<'declarations> Declarations<'declarations> {
    /// Where the value at `field` of the instance is declared.
    fn of(&self, field: &str) -> Option<&'declarations SourceLocation> {
        self.at?.get(&format!("{}/{field}", self.instance))
    }
}

/// The project an instance declares.
#[derive(Debug, Clone, Copy)]
struct Owner<'owner> {
    name: &'owner str,
    declarations: Declarations<'owner>,
    /// Where the instance's `name` is declared, which the stamp of every
    /// node of this project points at; `None` when the evaluation recorded
    /// nothing, and stamps are told apart by name alone.
    declared: Option<&'owner SourceLocation>,
}

impl<'owner> Owner<'owner> {
    fn new(name: &'owner str, declarations: Declarations<'owner>) -> Self {
        Self {
            name,
            declarations,
            declared: declarations.of("name"),
        }
    }

    /// Where the project named `project`, which the value at `field`
    /// holds, is declared, when it carries this project's name and is not
    /// this project; `None` for this project and for any other name. A
    /// stamp whose declaration was not recorded, while this instance's was,
    /// cannot be shown to be this project's, and is not.
    fn namesake(&self, project: &str, field: &str) -> Option<String> {
        if project != self.name {
            return None;
        }
        let own = self.declared?;
        match self.declarations.of(field) {
            Some(declared) if declared == own => None,
            Some(declared) => Some(format!("`{}`", declared.file)),
            None => Some("a place this evaluation did not record".to_string()),
        }
    }

    /// Whether the project named `project`, which the value at `field`
    /// holds, is this project.
    fn owns(&self, project: &str, field: &str) -> bool {
        project == self.name && self.namesake(project, field).is_none()
    }
}

/// Check the identities in a project instance's value, before reference
/// metadata names any dependency: the stamps of whole sequences are still
/// there to inspect. Every entry of another project is replaced by a
/// marker, which [`check_inputs`] removes once names are assigned. An
/// instance without a `name` is no project and is left alone.
pub(super) fn check_identities(
    value: &mut Value,
    declarations: Declarations<'_>,
) -> NodeDiagnostics {
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
        owner: Owner::new(&owner, declarations),
        diagnostics: NodeDiagnostics::default(),
    };
    if let Some(Value::Object(tasks)) = root.get_mut("tasks") {
        for (name, node) in tasks.iter_mut() {
            checks.task_node(name, node);
        }
    }
    if let Some(Value::Object(services)) = root.get_mut("services") {
        for (name, service) in services.iter_mut() {
            let problems = worded(
                "service",
                name,
                checks.service(service, &format!("services.{name}")),
            );
            record(&mut checks.diagnostics.services, name, problems);
        }
    }
    if let Some(Value::Object(images)) = root.get_mut("images") {
        for (name, image) in images.iter_mut() {
            let problems = worded(
                "image",
                name,
                checks.image(image, &format!("images.{name}")),
            );
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

/// Where the instance being checked lies, and the projects around it: what
/// a message about an input that leaves the project points at.
#[derive(Debug, Clone, Copy)]
pub(super) struct InstancePlace<'place> {
    /// The instance's directory relative to the module root (`.` for the
    /// root).
    pub(super) path: &'place Path,
    /// The declaring directory of every project name the evaluation knows,
    /// relative to the module root.
    pub(super) projects: &'place BTreeMap<String, PathBuf>,
    /// Where the instance's values are declared.
    pub(super) declarations: Declarations<'place>,
}

/// Remove what [`check_identities`] marked, check every task's inputs and
/// that every dependency is named, and write the problems found into the
/// value, where the project reads them. Runs once tasks carry their source
/// locations and reference metadata has named what it can.
pub(super) fn check_inputs(
    value: &mut Value,
    mut diagnostics: NodeDiagnostics,
    place: InstancePlace<'_>,
) {
    remove_marked(value);
    let Some(root) = value.as_object_mut() else {
        return;
    };
    let Some(owner) = root
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
    else {
        return;
    };
    let checks = InputChecks {
        owner: Owner::new(&owner, place.declarations),
        place,
    };
    if let Some(Value::Object(tasks)) = root.get_mut("tasks") {
        for (name, node) in tasks.iter_mut() {
            checks.task_node(name, node, &mut diagnostics.tasks);
        }
    }
    if let Some(Value::Object(images)) = root.get_mut("images") {
        for (name, image) in images.iter_mut() {
            if let Value::Object(object) = image {
                let problems = worded(
                    "image",
                    name,
                    checks.inputs(object, &format!("images.{name}")),
                );
                record(&mut diagnostics.images, name, problems);
            }
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

/// The checks of inputs and dependency names, once names are assigned.
struct InputChecks<'checks> {
    /// The project being checked.
    owner: Owner<'checks>,
    place: InstancePlace<'checks>,
}

impl InputChecks<'_> {
    /// Check the inputs and dependencies of the task, group or sequence
    /// called `name` and of everything below it.
    fn task_node(&self, name: &str, node: &mut Value, into: &mut BTreeMap<String, Vec<String>>) {
        match node {
            Value::Array(steps) => {
                for (index, step) in steps.iter_mut().enumerate() {
                    self.task_node(&format!("{name}[{index}]"), step, into);
                }
            }
            Value::Object(object) if is_group(object) => {
                let problems: Vec<String> = unnamed_dependencies(object)
                    .iter()
                    .map(|problem| for_task(name, None, problem))
                    .collect();
                record(into, name, problems);
                for (child, child_node) in object.iter_mut() {
                    if !GROUP_FIELDS.contains(&child.as_str()) {
                        self.task_node(&format!("{name}.{child}"), child_node, into);
                    }
                }
            }
            Value::Object(object) => {
                let source = source_of(object);
                let mut problems = unnamed_dependencies(object);
                problems.extend(self.inputs(object, &format!("tasks.{name}")));
                let worded: Vec<String> = problems
                    .iter()
                    .map(|problem| for_task(name, source.as_ref(), problem))
                    .collect();
                record(into, name, worded);
            }
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }

    /// Read every input of the task or image at `path` the way the rest of
    /// cuenv reads inputs: a task's `outputFiles` becomes a task-output
    /// input, and an input that cannot be read is removed, saying why.
    fn inputs(&self, object: &mut Map<String, Value>, path: &str) -> Vec<String> {
        let Some(inputs) = object.get_mut("inputs") else {
            return Vec::new();
        };
        let Value::Array(entries) = inputs else {
            let problem = "`inputs` is not a list".to_string();
            object.remove("inputs");
            return vec![problem];
        };
        let mut problems = Vec::new();
        let mut kept = Vec::with_capacity(entries.len());
        for (index, entry) in std::mem::take(entries).into_iter().enumerate() {
            match self.input(entry, &format!("{path}.inputs[{index}]")) {
                Ok(entry) => kept.push(entry),
                Err(problem) => problems.push(format!("inputs[{index}] {problem}")),
            }
        }
        *entries = kept;
        problems
    }

    /// The input at `path` as the rest of cuenv reads it, or why it cannot
    /// be read.
    fn input(&self, entry: Value, path: &str) -> Result<Value, String> {
        match &entry {
            Value::Null => Err("has no value".to_string()),
            Value::Object(object) if object.contains_key("cuenvFiles") => {
                file_reference_problem(object).map_or(Ok(entry), Err)
            }
            Value::Object(object) if object.contains_key("cuenvOutputFilesRef") => {
                self.output_files(object, path)
            }
            Value::Object(object) if object.contains_key("project") => {
                Err(removed_project_reference(object))
            }
            Value::String(path) => {
                if let Some(problem) = removed_shorthand(path) {
                    return Err(problem);
                }
                if has_parent_segment(path) {
                    return Err(self.leaves_project(path));
                }
                Ok(entry)
            }
            Value::Object(_) | Value::Bool(_) | Value::Number(_) | Value::Array(_) => Ok(entry),
        }
    }

    /// A task's `outputFiles`, at `path`, as the task-output input of the
    /// task it stamps (R5): by its name in this project, `#project:name` in
    /// another.
    fn output_files(&self, object: &Map<String, Value>, path: &str) -> Result<Value, String> {
        let text = |field: &str| {
            object
                .get(field)
                .and_then(Value::as_str)
                .unwrap_or_default()
        };
        let (project, task) = (text("cuenvProject"), text("cuenvTask"));
        if project.is_empty() {
            return Err(
                "reads the `outputFiles` of a task outside every project: tasks \
                        outside a project cannot be referenced; move it into a project"
                    .to_string(),
            );
        }
        if task.is_empty() {
            return Err(format!(
                "reads the `outputFiles` of a task of project '{project}' that has no name \
                 here (a step of a sequence): read the `outputFiles` of a named task"
            ));
        }
        let name = if self.owner.owns(project, &format!("{path}.cuenvProject")) {
            task.to_string()
        } else {
            format!("#{project}:{task}")
        };
        Ok(serde_json::json!({ "task": name }))
    }

    /// What to do about a string input that leaves the project (R0): how to
    /// name its files from the project that holds them, or from the module
    /// root.
    fn leaves_project(&self, input: &str) -> String {
        let rewrite = match module_relative(self.place.path, input) {
            Some(path) => self.files_spelling(&path),
            None => "it leaves the CUE module too, and nothing above the module root can be \
                     named"
                .to_string(),
        };
        format!(
            "`{input}` leaves the project directory, and `..` inputs are no longer read: \
             {rewrite}. Built files of another project are read through \
             `<pkg>.tasks.<name>.outputFiles`. On a schema that does not offer these \
             references yet, run `cue mod get github.com/cuenv/cuenv@latest` first"
        )
    }

    /// How a file at `path` (relative to the module root) is named from this
    /// instance: through the project whose directory holds it, else from the
    /// module root.
    fn files_spelling(&self, path: &Path) -> String {
        // A project cannot import a project of a directory above it, or
        // itself: that would be an import cycle.
        let holder = self
            .place
            .projects
            .iter()
            .filter(|(_, directory)| !directory.as_os_str().is_empty())
            .filter(|(_, directory)| path.starts_with(directory))
            .filter(|(_, directory)| !self.place.path.starts_with(directory))
            .max_by_key(|(name, directory)| (directory.components().count(), *name));
        let module_form = format!(
            "`schema.#ModuleFiles & {{paths: [\"{}\"]}}`",
            forward_slashes(path)
        );
        match holder {
            Some((name, directory)) => {
                let rest = path
                    .strip_prefix(directory)
                    .map(forward_slashes)
                    .unwrap_or_default();
                let rest = if rest.is_empty() {
                    "**".to_string()
                } else {
                    rest
                };
                format!(
                    "import project '{name}' from `{}` and write `<pkg>.#Files & {{paths: \
                     [\"{rest}\"]}}`, or read it from the module root with {module_form}",
                    forward_slashes(directory)
                )
            }
            None => format!("read it from the module root with {module_form}"),
        }
    }
}

/// Remove the dependencies of a task or group that no rule could name, and
/// say why each one was removed.
fn unnamed_dependencies(object: &mut Map<String, Value>) -> Vec<String> {
    let Some(Value::Array(entries)) = object.get_mut("dependsOn") else {
        return Vec::new();
    };
    let mut problems = Vec::new();
    let mut index = 0;
    entries.retain(|entry| {
        let named = match entry {
            Value::Object(fields) => fields
                .get("_name")
                .and_then(Value::as_str)
                .is_some_and(|name| !name.is_empty()),
            Value::String(name) => !name.is_empty(),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::Array(_) => false,
        };
        if !named {
            let stamped = entry.get("cuenvRef").is_some();
            problems.push(if stamped {
                format!(
                    "dependsOn[{index}] is a task outside every project: tasks outside a \
                     project cannot be referenced; move it into a project"
                )
            } else {
                format!("dependsOn[{index}] does not name a task of this project")
            });
        }
        index += 1;
        named
    });
    problems
}

/// Whether a string input has a `..` segment.
fn has_parent_segment(path: &str) -> bool {
    path.trim()
        .split(['/', '\\'])
        .any(|segment| segment == "..")
}

/// `input`, written in the project at `project_path`, as a path from the
/// module root; `None` when it leaves the module or is absolute.
fn module_relative(project_path: &Path, input: &str) -> Option<PathBuf> {
    let input = input.trim();
    if input.starts_with(['/', '\\']) {
        return None;
    }
    let mut resolved = PathBuf::new();
    for component in project_path.join(input).components() {
        match component {
            Component::Normal(part) => resolved.push(part),
            Component::ParentDir => {
                if !resolved.pop() {
                    return None;
                }
            }
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    Some(resolved)
}

fn forward_slashes(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// What replaces a `{project, task, map}` input, which cuenv no longer
/// reads.
fn removed_project_reference(object: &Map<String, Value>) -> String {
    let text = |field: &str| {
        object
            .get(field)
            .and_then(Value::as_str)
            .unwrap_or("<task>")
    };
    format!(
        "is a `{{project: \"{}\", task: \"{}\", map: [...]}}` reference, which cuenv no \
         longer reads: import the project and read `<pkg>.tasks.{}.outputFiles` (it also \
         depends on that task), or `<pkg>.#Files & {{paths: [...]}}` for files no task builds. \
         Run `cue mod get github.com/cuenv/cuenv@latest` first if your schema does not offer \
         them",
        text("project"),
        text("task"),
        text("task")
    )
}

/// What replaces the `#project:task:path` and `#project:task` input
/// shorthands, which cuenv no longer reads; `None` for any other string.
fn removed_shorthand(input: &str) -> Option<String> {
    let reference = input.strip_prefix('#')?;
    let (project, rest) = reference.split_once(':')?;
    if project.is_empty() || project.contains(['/', '\\']) || rest.is_empty() {
        return None;
    }
    let task = rest.split(':').next().unwrap_or(rest);
    Some(format!(
        "is the `{input}` shorthand, which cuenv no longer reads: import project '{project}' \
         and read `<pkg>.tasks.{task}.outputFiles` (it also depends on that task), or \
         `<pkg>.#Files & {{paths: [...]}}` for files no task builds. Run `cue mod get \
         github.com/cuenv/cuenv@latest` first if your schema does not offer them"
    ))
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
    /// Where its project is declared, when that project carries this
    /// project's name and is not this project.
    namesake: Option<String>,
}

impl std::fmt::Display for Identity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = if self.kind.is_empty() {
            "node"
        } else {
            &self.kind
        };
        if self.name.is_empty() {
            write!(formatter, "a {kind} of project '{}'", self.project)?;
        } else {
            write!(
                formatter,
                "{kind} '{}' of project '{}'",
                self.name, self.project
            )?;
        }
        match &self.namesake {
            Some(declared) => write!(
                formatter,
                " declared in {declared}, a namesake of this project"
            ),
            None => Ok(()),
        }
    }
}

struct NodeChecks<'owner> {
    owner: Owner<'owner>,
    diagnostics: NodeDiagnostics,
}

impl NodeChecks<'_> {
    /// The identity the stamp of `value`, the value at `path`, gives, when
    /// it names a project.
    fn stamped(&self, value: &Value, path: &str) -> Option<Identity> {
        let stamp = value.get("cuenvRef")?;
        let text = |field: &str| {
            stamp
                .get(field)
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        };
        let project = text("project");
        if project.is_empty() {
            return None;
        }
        let namesake = self
            .owner
            .namesake(&project, &format!("{path}.cuenvRef.project"));
        Some(Identity {
            kind: text("kind"),
            project,
            name: text("name"),
            namesake,
        })
    }

    /// Whether the node `identity` names is a node of this project.
    fn owns(&self, identity: &Identity) -> bool {
        identity.project == self.owner.name && identity.namesake.is_none()
    }

    /// The identity of `value`, the value at `path`, when it belongs to
    /// another project. A sequence belongs to another project when any
    /// member does.
    fn foreign(&self, value: &Value, path: &str) -> Option<Identity> {
        match value {
            Value::Array(members) => members
                .iter()
                .enumerate()
                .find_map(|(index, member)| self.foreign(member, &format!("{path}[{index}]"))),
            Value::Object(_) => self
                .stamped(value, path)
                .filter(|identity| !self.owns(identity)),
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
                let path = format!("tasks.{name}");
                let mut problems = self.task_dependencies(object, &path);
                if is_group(object) {
                    for (child, child_node) in object.iter_mut() {
                        if !GROUP_FIELDS.contains(&child.as_str()) {
                            self.task_node(&format!("{name}.{child}"), child_node);
                        }
                    }
                } else {
                    problems.extend(self.output_references(object, &path));
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

    /// Name the entries of a task's or group's `dependsOn` by their stamps:
    /// a node of this project by its name (R1), a task or group of another
    /// project as `#project:name` (R2). What a stamp cannot name is removed,
    /// saying why. An entry whose stamp names no project, or that has no
    /// stamp (an older pinned schema), is left to reference metadata.
    fn task_dependencies(&self, object: &mut Map<String, Value>, path: &str) -> Vec<String> {
        let Some(Value::Array(entries)) = object.get_mut("dependsOn") else {
            return Vec::new();
        };
        let mut problems = Vec::new();
        for (index, entry) in entries.iter_mut().enumerate() {
            let entry_path = format!("{path}.dependsOn[{index}]");
            if let Some(problem) = self.dependency(index, entry, &entry_path) {
                problems.push(problem);
                *entry = marker();
            }
        }
        problems
    }

    /// Name one `dependsOn` entry, the value at `path`, by its stamp, or say
    /// why it cannot be a dependency.
    fn dependency(&self, index: usize, entry: &mut Value, path: &str) -> Option<String> {
        if entry.is_array() {
            // A whole sequence has no stamp of its own and no name another
            // project could know it by.
            return self.foreign(entry, path).map(|identity| {
                format!(
                    "dependsOn[{index}] is a sequence holding {identity}: depend on a named task \
                     or group of project '{}'",
                    identity.project
                )
            });
        }
        let identity = self.stamped(entry, path)?;
        let local = self.owns(&identity);
        let name = if local {
            if identity.name.is_empty() {
                // A step of a sequence of this project: the bridge names it
                // through reference metadata.
                return None;
            }
            identity.name.clone()
        } else {
            if !matches!(identity.kind.as_str(), "task" | "group") {
                return Some(format!(
                    "dependsOn[{index}] is {identity}: not supported across projects yet"
                ));
            }
            if identity.name.is_empty() {
                return Some(format!(
                    "dependsOn[{index}] is {identity} with no name here (a step of a \
                     sequence): depend on a named task or group of project '{}'",
                    identity.project
                ));
            }
            format!("#{}:{}", identity.project, identity.name)
        };
        if let Value::Object(fields) = entry {
            fields.insert("_name".to_string(), Value::String(name));
        }
        None
    }

    /// Mark the entries of the list `field` of `object`, the value at
    /// `path`, that belong to another project for removal, returning each
    /// with its index.
    fn foreign_entries(
        &self,
        object: &mut Map<String, Value>,
        field: &str,
        path: &str,
    ) -> Vec<(usize, Identity)> {
        let Some(Value::Array(entries)) = object.get_mut(field) else {
            return Vec::new();
        };
        let mut found = Vec::new();
        for (index, entry) in entries.iter_mut().enumerate() {
            if let Some(identity) = self.foreign(entry, &format!("{path}.{field}[{index}]")) {
                found.push((index, identity));
                *entry = marker();
            }
        }
        found
    }

    /// Problems with output references to another project's tasks or images
    /// anywhere in the node at `path`, other than in its own computed
    /// references and in the nodes it embeds.
    fn output_references(&self, object: &Map<String, Value>, path: &str) -> Vec<String> {
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
            self.collect_output_references(value, &format!("{path}.{field}"), &mut problems);
        }
        problems
    }

    /// Problems with output references to another project's tasks or images
    /// in `value`, the value at `path`.
    fn collect_output_references(&self, value: &Value, path: &str, problems: &mut Vec<String>) {
        match value {
            Value::Object(object) if is_output_reference(object) => {
                let project = object
                    .get("cuenvProject")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                if !project.is_empty() && !self.owner.owns(project, &format!("{path}.cuenvProject"))
                {
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
                for (field, nested) in object {
                    self.collect_output_references(nested, &format!("{path}.{field}"), problems);
                }
            }
            Value::Array(items) => {
                for (index, item) in items.iter().enumerate() {
                    self.collect_output_references(item, &format!("{path}[{index}]"), problems);
                }
            }
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }

    /// Problems of the service at `path`.
    fn service(&self, service: &mut Value, path: &str) -> Vec<String> {
        let Value::Object(object) = service else {
            return Vec::new();
        };
        let mut problems = self.unsupported(object, "dependsOn", path);
        if let Some(Value::Object(watch)) = object.get_mut("watch") {
            problems.extend(self.unsupported(watch, "rebuild", &format!("{path}.watch")));
        }
        problems.extend(self.output_references(object, path));
        problems
    }

    /// Problems of the image at `path`.
    fn image(&self, image: &mut Value, path: &str) -> Vec<String> {
        let Value::Object(object) = image else {
            return Vec::new();
        };
        let mut problems = self.unsupported(object, "dependsOn", path);
        problems.extend(self.output_references(object, path));
        problems
    }

    fn pipeline(&self, name: &str, pipeline: &mut Value) -> Vec<String> {
        let Some(Value::Array(entries)) = pipeline.get_mut("tasks") else {
            return Vec::new();
        };
        let mut problems = Vec::new();
        for (index, entry) in entries.iter_mut().enumerate() {
            let path = format!("ci.pipelines.{name}.tasks[{index}]");
            // A matrix entry names its task in `task`.
            let (named, path) = match entry
                .as_object()
                .filter(|object| !object.contains_key("cuenvRef"))
                .and_then(|object| object.get("task"))
            {
                Some(task) => (task, format!("{path}.task")),
                None => (&*entry, path),
            };
            if let Some(identity) = self.foreign(named, &path) {
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

    /// Remove the entries of `field` of the node at `path` that belong to
    /// another project, where no cross-project reference is supported yet
    /// (R6).
    fn unsupported(&self, object: &mut Map<String, Value>, field: &str, path: &str) -> Vec<String> {
        self.foreign_entries(object, field, path)
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

    /// The projects of the module the checked instance (`app`) lies in.
    fn module_projects() -> BTreeMap<String, PathBuf> {
        BTreeMap::from([
            ("app".to_string(), PathBuf::from("app")),
            ("content".to_string(), PathBuf::from("content")),
            ("site".to_string(), PathBuf::new()),
        ])
    }

    /// An evaluation that recorded no declarations: stamps are told apart
    /// by project name alone.
    const UNRECORDED: Declarations<'static> = Declarations {
        instance: "app",
        at: None,
    };

    fn check_declared(value: &mut Value, declarations: Declarations<'_>) {
        let diagnostics = check_identities(value, declarations);
        let projects = module_projects();
        check_inputs(
            value,
            diagnostics,
            InstancePlace {
                path: Path::new("app"),
                projects: &projects,
                declarations,
            },
        );
    }

    fn check_nodes(value: &mut Value) {
        check_declared(value, UNRECORDED);
    }

    fn checked(mut value: Value) -> Project {
        check_nodes(&mut value);
        serde_json::from_value(value).expect("the project loads")
    }

    #[test]
    fn a_dependency_is_named_by_its_stamp() {
        let mut consumer = task("app", "deploy");
        consumer["dependsOn"] = json!([
            task("content", "build"),
            task("app", "lint"),
            task("content", "checks.unit"),
        ]);
        let project = checked(json!({
            "name": "app",
            "tasks": {"build": task("app", "build"), "lint": task("app", "lint"), "deploy": consumer},
        }));

        let deploy = project.tasks["deploy"].as_task().expect("a task");
        let names: Vec<&str> = deploy.dependency_names().collect();
        assert_eq!(
            names,
            ["#content:build", "lint", "#content:checks.unit"],
            "another project's build is never this project's build"
        );
        assert!(project.diagnostics.task("deploy").is_empty());
    }

    #[test]
    fn a_namesake_project_of_another_package_is_another_project() {
        // `app` imports a package from outside its module that is also
        // named `app`: its stamps name `app`, but its `name` is declared
        // elsewhere.
        let at = |file: &str, line: u32| SourceLocation {
            file: file.to_string(),
            line,
            column: 0,
        };
        let own = at("app/env.cue", 9);
        let vendored = at("cue.mod/pkg/example.com/other/app/env.cue", 6);
        let mut consumer = task("app", "deploy");
        consumer["dependsOn"] = json!([
            task("app", "build"),
            task("app", "build"),
            task("app", "lint")
        ]);
        consumer["inputs"] = json!([
            {"cuenvOutputFilesRef": true, "cuenvProject": "app", "cuenvTask": "build"},
            {"cuenvOutputFilesRef": true, "cuenvProject": "app", "cuenvTask": "build"},
        ]);
        consumer["env"] = json!({
            "BUILT": {"cuenvOutputRef": true, "cuenvProject": "app", "cuenvTask": "build", "cuenvOutput": "stdout"},
        });
        let declarations: SourceMap = [
            ("app/name", &own),
            ("app/tasks.deploy.dependsOn[0].cuenvRef.project", &vendored),
            ("app/tasks.deploy.dependsOn[1].cuenvRef.project", &own),
            ("app/tasks.deploy.inputs[0].cuenvProject", &vendored),
            ("app/tasks.deploy.inputs[1].cuenvProject", &own),
            ("app/tasks.deploy.env.BUILT.cuenvProject", &vendored),
            (
                "app/ci.pipelines.default.tasks[0].cuenvRef.project",
                &vendored,
            ),
        ]
        .into_iter()
        .map(|(key, location)| (key.to_string(), location.clone()))
        .collect();
        let mut value = json!({
            "name": "app",
            "tasks": {"build": task("app", "build"), "lint": task("app", "lint"), "deploy": consumer},
            "ci": {"pipelines": {"default": {"tasks": [task("app", "build")]}}},
        });
        check_declared(
            &mut value,
            Declarations {
                instance: "app",
                at: Some(&declarations),
            },
        );
        let project: Project = serde_json::from_value(value).expect("the project loads");

        let deploy = project.tasks["deploy"].as_task().expect("a task");
        // The namesake's `build` is bound among the imports, where a project
        // of another module never binds; a stamp whose declaration was not
        // recorded cannot be shown to be this project's.
        assert_eq!(
            deploy.dependency_names().collect::<Vec<_>>(),
            ["#app:build", "build", "#app:lint"]
        );
        assert_eq!(
            deploy.iter_task_output_names().collect::<Vec<_>>(),
            ["#app:build", "build"]
        );
        let problems = project.diagnostics.task("deploy");
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(
            problems[0]
                .contains("`build.stdout` of project 'app' is not supported across projects"),
            "{problems:?}"
        );
        let pipeline = project.diagnostics.pipeline("default");
        assert_eq!(pipeline.len(), 1, "{pipeline:?}");
        assert!(
            pipeline[0].contains(
                "task 'build' of project 'app' declared in \
                 `cue.mod/pkg/example.com/other/app/env.cue`, a namesake of this project"
            ),
            "{pipeline:?}"
        );
    }

    #[test]
    fn a_stamp_names_a_dependency_before_reference_metadata() {
        // Reference metadata names what the bridge saw, which a `let` or a
        // re-export can spell any way; the stamp says what the value is.
        let mut value = json!({
            "name": "app",
            "tasks": {"deploy": {"command": "true", "dependsOn": [task("content", "build")]}},
        });
        let diagnostics = check_identities(&mut value, UNRECORDED);
        assert!(diagnostics.is_empty());
        assert_eq!(
            value["tasks"]["deploy"]["dependsOn"][0]["_name"],
            json!("#content:build")
        );
    }

    #[test]
    fn what_another_project_cannot_be_named_by_is_a_problem_of_the_node() {
        let mut group = json!({
            "type": "group",
            "cuenvRef": {"kind": "group", "project": "app", "name": "checks"},
            "dependsOn": [[task("app", "a"), [task("content", "b")]]],
            "unit": task("app", "checks.unit"),
        });
        group["unit"]["dependsOn"] = json!([
            task("content", ""),
            {"cuenvRef": {"kind": "image", "project": "content", "name": "base"}, "type": "image"},
            task("content", "build"),
        ]);
        let project = checked(json!({"name": "app", "tasks": {"checks": group}}));

        let problems = project.diagnostics.task("checks");
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(
            problems[0].contains("is a sequence holding task 'b' of project 'content'"),
            "{problems:?}"
        );
        assert!(problems[0].contains("depend on a named task or group of project 'content'"));
        let unit = project.diagnostics.task("checks.unit");
        assert_eq!(unit.len(), 2, "{unit:?}");
        assert!(
            unit[0].contains("with no name here (a step of a sequence)"),
            "{unit:?}"
        );
        assert!(
            unit[1].contains("image 'base' of project 'content': not supported across projects"),
            "{unit:?}"
        );
        let checks = project.tasks["checks"].as_group().expect("a group");
        assert!(checks.depends_on.is_empty());
        let unit = checks.children["unit"].as_task().expect("a task");
        assert_eq!(
            unit.dependency_names().collect::<Vec<_>>(),
            ["#content:build"]
        );
    }

    #[test]
    fn a_task_outside_every_project_is_named_only_by_reference_metadata() {
        // `#Svc.tasks.migrate` names a local task through reference metadata
        // (R3); with no metadata it names nothing, and says so.
        let mut consumer = task("app", "deploy");
        consumer["dependsOn"] = json!([
            {"_name": "migrate", "cuenvRef": {"kind": "task", "project": "", "name": ""}, "command": "true"},
            {"cuenvRef": {"kind": "task", "project": "", "name": ""}, "command": "true"},
        ]);
        let project = checked(json!({
            "name": "app",
            "tasks": {"migrate": task("app", "migrate"), "deploy": consumer},
        }));

        let deploy = project.tasks["deploy"].as_task().expect("a task");
        assert_eq!(deploy.dependency_names().collect::<Vec<_>>(), ["migrate"]);
        let problems = project.diagnostics.task("deploy");
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(
            problems[0].contains("dependsOn[1] is a task outside every project"),
            "{problems:?}"
        );
    }

    #[test]
    fn output_files_read_as_the_task_output_of_the_task_they_stamp() {
        let mut consumer = task("app", "deploy");
        consumer["inputs"] = json!([
            {"cuenvOutputFilesRef": true, "cuenvProject": "content", "cuenvTask": "build"},
            {"cuenvOutputFilesRef": true, "cuenvProject": "app", "cuenvTask": "docs.build"},
            {"cuenvOutputFilesRef": true, "cuenvProject": "content", "cuenvTask": ""},
            {"cuenvOutputFilesRef": true, "cuenvProject": "", "cuenvTask": "migrate"},
        ]);
        let project = checked(json!({"name": "app", "tasks": {"deploy": consumer}}));

        let deploy = project.tasks["deploy"].as_task().expect("a task");
        let outputs: Vec<&str> = deploy.iter_task_output_names().collect();
        assert_eq!(outputs, ["#content:build", "docs.build"]);
        let problems = project.diagnostics.task("deploy");
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(problems[0].contains("inputs[2] reads the `outputFiles` of a task of project"));
        assert!(problems[1].contains("inputs[3] reads the `outputFiles` of a task outside"));
    }

    #[test]
    fn an_input_that_leaves_the_project_names_its_rewrite() {
        let mut consumer = task("app", "build");
        consumer["_source"] = json!({"file": "app/env.cue", "line": 4, "column": 2});
        consumer["inputs"] = json!([
            "src/**",
            "../content/articles/**",
            "../bun.lock",
            "../../outside.txt",
            "a..b/c",
        ]);
        let project = checked(json!({"name": "app", "tasks": {"build": consumer}}));

        let problems = project.diagnostics.task("build");
        assert_eq!(problems.len(), 3, "{problems:?}");
        assert!(
            problems[0]
                .starts_with("task 'build' (app/env.cue:4): inputs[1] `../content/articles/**`"),
            "{problems:?}"
        );
        assert!(
            problems[0].contains("import project 'content' from `content` and write `<pkg>.#Files & {paths: [\"articles/**\"]}`"),
            "{problems:?}"
        );
        assert!(
            problems[0].contains("`schema.#ModuleFiles & {paths: [\"content/articles/**\"]}`"),
            "{problems:?}"
        );
        // The root project is above `app`: importing it would be a cycle.
        assert!(
            problems[1].contains(
                "read it from the module root with `schema.#ModuleFiles & {paths: [\"bun.lock\"]}`"
            ),
            "{problems:?}"
        );
        assert!(problems[2].contains("nothing above the module root can be named"));
        let build = project.tasks["build"].as_task().expect("a task");
        assert_eq!(build.inputs.len(), 2, "`src/**` and `a..b/c` remain");
    }

    #[test]
    fn the_removed_cross_project_forms_fail_their_task() {
        let mut consumer = task("app", "build");
        consumer["inputs"] = json!([
            {"project": "content", "task": "build", "map": [{"from": "dist", "to": "dist"}]},
            "#content:build:dist/app.js",
            "#content:build",
            "#not-a-reference",
        ]);
        let project = checked(json!({"name": "app", "tasks": {"build": consumer}}));

        let problems = project.diagnostics.task("build");
        assert_eq!(problems.len(), 3, "{problems:?}");
        assert!(problems[0].contains("inputs[0] is a `{project: \"content\", task: \"build\""));
        assert!(problems[0].contains("`<pkg>.tasks.build.outputFiles`"));
        assert!(problems[1].contains("inputs[1] is the `#content:build:dist/app.js` shorthand"));
        assert!(problems[2].contains("inputs[2] is the `#content:build` shorthand"));
        let build = project.tasks["build"].as_task().expect("a task");
        assert_eq!(
            build.inputs.len(),
            1,
            "a path that only starts with `#` remains"
        );
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
