//! Named contexts: select environment variables for each invocation.
//!
//! `contexts.<name>` is a filter (see `schema/context.cue`): a [`When`] over
//! the facts of an invocation, or a [`TaskFilter`] over the task being run.
//! This module builds the facts, decides which contexts are active and
//! selects `env.context.<name>` accordingly.
//!
//! A filter is data. A field left out matches anything, and a string is a
//! regular expression that must match the whole value, so a string without
//! regular expression characters matches exactly itself.
//!
//! Everything a filter reads is reported by the machine running cuenv, so a
//! context chooses what an invocation asks for. It never establishes that the
//! invocation is entitled to it: the secrets backend has to enforce that.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

pub use cuenv_manifest::context::{ContextDefinition, TaskFilter, When};
use cuenv_manifest::environment::{Env, EnvSelection, EnvValue};
use cuenv_manifest::manifest::InfrastructurePolicyAction;
use cuenv_manifest::tasks::Task;
use regex_automata::Anchored;
use regex_automata::dfa::{Automaton, StartKind, dense};
use regex_automata::util::start;
use serde_json::{Map, Value, json};

use crate::{Error, Result};

mod task_selection;

pub use task_selection::{TaskEnv, TaskEnvKey, TaskSelection, TaskSelectionOptions};

/// The outcome of reading a filter, with the problem in words. [`validate`]
/// names the context when it reports one.
type Checked<T> = std::result::Result<T, String>;

/// The values of the `consumer` fact.
const CONSUMERS: [&str; 4] = ["task", "exec", "infrastructure", "shell"];

/// A task about to receive the environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskSubject<'a> {
    /// The task's full name.
    pub name: &'a str,
    /// The program the task runs; `None` for a script.
    pub command: Option<&'a str>,
    /// The task's labels.
    pub labels: &'a [String],
}

impl<'a> TaskSubject<'a> {
    /// The subject for a task of the graph.
    #[must_use]
    pub fn of(name: &'a str, task: &'a Task) -> Self {
        Self {
            name,
            command: (!task.command.is_empty()).then_some(task.command.as_str()),
            labels: &task.labels,
        }
    }

    /// The subject for a name that is not one task, such as a group.
    #[must_use]
    pub const fn named(name: &'a str) -> Self {
        Self {
            name,
            command: None,
            labels: &[],
        }
    }
}

/// What is about to receive the environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consumer<'a> {
    /// A task.
    Task(TaskSubject<'a>),
    /// A command run by `cuenv exec`: the program as it was written on the
    /// command line, which is also what an `allowExec` policy names.
    Exec(&'a str),
    /// An infrastructure action.
    Infrastructure(InfrastructurePolicyAction),
    /// The shell, or a command that prints the environment.
    Shell,
}

impl Consumer<'_> {
    /// The value of the `consumer` fact.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Task(_) => "task",
            Self::Exec(_) => "exec",
            Self::Infrastructure(_) => "infrastructure",
            Self::Shell => "shell",
        }
    }
}

/// The CI run cuenv is part of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CiFacts {
    /// `github`, `gitlab`, `buildkite` or `unknown`.
    pub provider: String,
    /// The triggering event, as the provider names it.
    pub event: Option<String>,
    /// The ref the run is for, as the provider names it.
    pub reference: Option<String>,
    /// Who triggered the run.
    pub actor: Option<String>,
}

impl CiFacts {
    /// Read the CI run from environment variables; `None` outside CI.
    pub fn from_variables(variable: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let set = |name: &str| variable(name).filter(|value| !value.is_empty());
        let enabled = |name: &str| set(name).is_some_and(|value| value != "false" && value != "0");
        let (provider, names) = if enabled("GITHUB_ACTIONS") {
            (
                "github",
                ["GITHUB_EVENT_NAME", "GITHUB_REF", "GITHUB_ACTOR"],
            )
        } else if enabled("GITLAB_CI") {
            (
                "gitlab",
                [
                    "CI_PIPELINE_SOURCE",
                    "CI_COMMIT_REF_NAME",
                    "GITLAB_USER_LOGIN",
                ],
            )
        } else if enabled("BUILDKITE") {
            (
                "buildkite",
                [
                    "BUILDKITE_SOURCE",
                    "BUILDKITE_BRANCH",
                    "BUILDKITE_BUILD_CREATOR",
                ],
            )
        } else if enabled("CI") {
            return Some(Self {
                provider: "unknown".to_string(),
                event: None,
                reference: None,
                actor: None,
            });
        } else {
            return None;
        };
        let [event, reference, actor] = names;
        Some(Self {
            provider: provider.to_string(),
            event: set(event),
            reference: set(reference),
            actor: set(actor),
        })
    }
}

/// The facts of an invocation that do not depend on the consumer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    /// The CI run, or `None` outside CI.
    pub ci: Option<CiFacts>,
    /// `git config user.name`.
    pub user_name: Option<String>,
    /// `git config user.email`.
    pub user_email: Option<String>,
    /// The operating system (`std::env::consts::OS`).
    pub host_os: String,
    /// The architecture (`std::env::consts::ARCH`).
    pub host_arch: String,
    /// The host's name.
    pub host_name: Option<String>,
    /// The checked-out branch.
    pub branch: Option<String>,
}

impl Facts {
    /// Detect the facts for a project directory. Detection opens the git
    /// repository, so the result is kept for the life of the process.
    #[must_use]
    pub fn detect(directory: &Path) -> Arc<Self> {
        static DETECTED: OnceLock<Mutex<HashMap<PathBuf, Arc<Facts>>>> = OnceLock::new();
        let detected = DETECTED.get_or_init(Mutex::default);
        if let Ok(known) = detected.lock()
            && let Some(facts) = known.get(directory)
        {
            return Arc::clone(facts);
        }
        let repository = gix::discover(directory).ok();
        let setting = |key: &str| {
            let value = repository.as_ref()?.config_snapshot().string(key)?;
            non_empty(&value.to_string())
        };
        let facts = Arc::new(Self {
            ci: CiFacts::from_variables(|name| std::env::var(name).ok()),
            user_name: setting("user.name"),
            user_email: setting("user.email"),
            host_os: std::env::consts::OS.to_string(),
            host_arch: std::env::consts::ARCH.to_string(),
            host_name: host_name(),
            branch: repository.as_ref().and_then(branch),
        });
        if let Ok(mut known) = detected.lock() {
            known.insert(directory.to_path_buf(), Arc::clone(&facts));
        }
        facts
    }

    /// The facts a [`When`] filters, for one consumer. Every fact is present;
    /// one that does not apply is `null`.
    #[must_use]
    pub fn document(&self, consumer: Consumer<'_>) -> Value {
        let (exec, infrastructure) = match consumer {
            Consumer::Exec(program) => (json!(program), Value::Null),
            Consumer::Infrastructure(action) => (Value::Null, json!(action)),
            Consumer::Task(_) | Consumer::Shell => (Value::Null, Value::Null),
        };
        json!({
            "consumer": consumer.name(),
            "exec": exec,
            "infrastructure": infrastructure,
            "ci": self.ci.as_ref().map(|ci| json!({
                "provider": ci.provider,
                "event": ci.event,
                "ref": ci.reference,
                "actor": ci.actor,
            })),
            "user": {"name": self.user_name, "email": self.user_email},
            "host": {"os": self.host_os, "arch": self.host_arch, "name": self.host_name},
            "vcs": {"branch": self.branch},
        })
    }
}

/// The trimmed text, unless nothing is left of it.
fn non_empty(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// The branch `HEAD` names; `None` when it is detached.
fn branch(repository: &gix::Repository) -> Option<String> {
    let name = repository.head_name().ok()??;
    non_empty(&name.shorten().to_string())
}

/// The name the operating system has for this host.
#[cfg(unix)]
fn host_name() -> Option<String> {
    non_empty(&rustix::system::uname().nodename().to_string_lossy())
}

/// The name the operating system has for this host.
#[cfg(not(unix))]
fn host_name() -> Option<String> {
    std::env::var("COMPUTERNAME")
        .ok()
        .as_deref()
        .and_then(non_empty)
}

/// Compile a filter string: a regular expression that must match the whole
/// value. An expression is compiled once for the life of the process: the
/// contexts are decided again for every task of a graph.
fn pattern(text: &str) -> Checked<regex::Regex> {
    static COMPILED: OnceLock<Mutex<HashMap<String, regex::Regex>>> = OnceLock::new();
    let compiled = COMPILED.get_or_init(Mutex::default);
    if let Ok(known) = compiled.lock()
        && let Some(expression) = known.get(text)
    {
        return Ok(expression.clone());
    }
    let invalid = |error| format!("`{text}` is not a valid regular expression: {error}");
    // The string must be an expression on its own. Otherwise `a)|(b` would
    // close the group below and stop being held to the whole value.
    regex::Regex::new(text).map_err(invalid)?;
    let expression = regex::Regex::new(&whole_value(text)).map_err(invalid)?;
    if let Ok(mut known) = compiled.lock() {
        known.insert(text.to_string(), expression.clone());
    }
    Ok(expression)
}

/// The expression that holds a filter string to the whole value.
fn whole_value(text: &str) -> String {
    format!("^(?:{text})$")
}

/// Whether a filter string matches exactly itself.
fn is_literal(text: &str) -> bool {
    regex::escape(text) == text
}

/// The most memory one filter string's automaton may take.
const AUTOMATON_BYTES: usize = 4 << 20;

/// The most pairs of states [`expressions_exclusive`] visits before it gives
/// up.
const AUTOMATON_PAIRS: usize = 1 << 18;

/// The automaton that accepts the values a filter string matches; `None` for
/// an expression too large to build one for.
fn automaton(text: &str) -> Option<dense::DFA<Vec<u32>>> {
    dense::Builder::new()
        .configure(
            dense::Config::new()
                .start_kind(StartKind::Anchored)
                .dfa_size_limit(Some(AUTOMATON_BYTES))
                .determinize_size_limit(Some(AUTOMATON_BYTES)),
        )
        .build(&whole_value(text))
        .ok()
}

/// Whether no value satisfies both filter strings, decided by walking their
/// automata together: a value both accept is a path that ends in a match in
/// each. `false` means only that it was not proved.
fn expressions_exclusive(first: &str, second: &str) -> bool {
    let (Some(first), Some(second)) = (automaton(first), automaton(second)) else {
        return false;
    };
    let anchored = start::Config::new().anchored(Anchored::Yes);
    let (Ok(first_start), Ok(second_start)) =
        (first.start_state(&anchored), second.start_state(&anchored))
    else {
        return false;
    };
    let mut seen = HashSet::from([(first_start, second_start)]);
    let mut pending = vec![(first_start, second_start)];
    while let Some((first_state, second_state)) = pending.pop() {
        if first.is_match_state(first.next_eoi_state(first_state))
            && second.is_match_state(second.next_eoi_state(second_state))
        {
            return false;
        }
        for byte in 0..=u8::MAX {
            let next = (
                first.next_state(first_state, byte),
                second.next_state(second_state, byte),
            );
            if first.is_dead_state(next.0) || second.is_dead_state(next.1) {
                continue;
            }
            if first.is_quit_state(next.0)
                || second.is_quit_state(next.1)
                || seen.len() >= AUTOMATON_PAIRS
            {
                return false;
            }
            if seen.insert(next) {
                pending.push(next);
            }
        }
    }
    true
}

/// Whether a fact satisfies its filter.
fn fact_matches(filter: &Value, fact: &Value) -> Checked<bool> {
    match (filter, fact) {
        (Value::Null, fact) => Ok(fact.is_null()),
        (Value::String(text), Value::String(fact)) => Ok(pattern(text)?.is_match(fact)),
        (Value::Object(filters), Value::Object(facts)) => facts_match(filters, facts),
        _ => Ok(false),
    }
}

/// Whether every filtered fact is present and satisfies its filter.
fn facts_match(filters: &Map<String, Value>, facts: &Map<String, Value>) -> Checked<bool> {
    for (name, filter) in filters {
        let Some(fact) = facts.get(name) else {
            return Ok(false);
        };
        if !fact_matches(filter, fact)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Whether a task satisfies a filter.
fn task_matches(filter: &TaskFilter, task: TaskSubject<'_>) -> Checked<bool> {
    if let Some(name) = &filter.name
        && !pattern(name)?.is_match(task.name)
    {
        return Ok(false);
    }
    if let Some(command) = &filter.command {
        let Some(program) = task.command else {
            return Ok(false);
        };
        if !pattern(command)?.is_match(program) {
            return Ok(false);
        }
    }
    for label in &filter.labels {
        let label = pattern(label)?;
        if !task.labels.iter().any(|carried| label.is_match(carried)) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Whether a context is active for a consumer whose facts are `document`.
fn is_active(
    definition: &ContextDefinition,
    consumer: Consumer<'_>,
    document: &Value,
) -> Checked<bool> {
    match (definition, consumer) {
        (ContextDefinition::When(when), _) => Ok(document
            .as_object()
            .map(|facts| facts_match(&when.0, facts))
            .transpose()?
            .unwrap_or(false)),
        (ContextDefinition::Task(filter), Consumer::Task(task)) => task_matches(filter, task),
        (ContextDefinition::Task(_), _) => Ok(false),
    }
}

/// The names of the contexts active for a consumer, sorted.
///
/// # Errors
///
/// Returns an error if a filter holds an invalid regular expression.
pub fn active_contexts(
    definitions: &HashMap<String, ContextDefinition>,
    facts: &Facts,
    consumer: Consumer<'_>,
) -> Result<Vec<String>> {
    let document = facts.document(consumer);
    let mut active = Vec::new();
    for (name, definition) in definitions {
        if is_active(definition, consumer, &document)
            .map_err(|problem| problem_in(name, &problem))?
        {
            active.push(name.clone());
        }
    }
    active.sort();
    Ok(active)
}

fn problem_in(name: &str, problem: &str) -> Error {
    Error::configuration(format!("contexts.{name}: {problem}"))
}

/// The fields of the facts at a path, or `None` for a fact that has no fields.
fn document_fields(path: &[&str]) -> Option<&'static [&'static str]> {
    match path {
        [] => Some(&[
            "consumer",
            "exec",
            "infrastructure",
            "ci",
            "user",
            "host",
            "vcs",
        ]),
        ["ci"] => Some(&["provider", "event", "ref", "actor"]),
        ["user"] => Some(&["name", "email"]),
        ["host"] => Some(&["os", "arch", "name"]),
        ["vcs"] => Some(&["branch"]),
        _ => None,
    }
}

/// Every value of a fact that has a fixed set of them.
fn known_values(path: &[&str]) -> Option<Vec<&'static str>> {
    match path {
        ["consumer"] => Some(CONSUMERS.to_vec()),
        ["infrastructure"] => Some(
            InfrastructurePolicyAction::ALL
                .iter()
                .map(|action| action.as_str())
                .collect(),
        ),
        _ => None,
    }
}

/// Reject a filter over a fact an invocation does not have: it would be a
/// context that never matches.
fn check_facts<'filter>(
    filters: &'filter Map<String, Value>,
    path: &mut Vec<&'filter str>,
) -> Checked<()> {
    let known = document_fields(path).unwrap_or_default();
    for (name, filter) in filters {
        path.push(name);
        if !known.contains(&name.as_str()) {
            return Err(format!(
                "`{}` is not a fact of an invocation",
                path.join(".")
            ));
        }
        check_fact(filter, path)?;
        path.pop();
    }
    Ok(())
}

/// Reject a filter that is not a string, `null` or fields, a string that is
/// not a regular expression, and one that matches none of a fact's values.
fn check_fact<'filter>(filter: &'filter Value, path: &mut Vec<&'filter str>) -> Checked<()> {
    match filter {
        Value::Null => Ok(()),
        Value::String(text) => {
            let expression = pattern(text)?;
            match known_values(path) {
                Some(values) if !values.iter().any(|value| expression.is_match(value)) => {
                    Err(format!(
                        "`{text}` matches no value of `{}` ({})",
                        path.join("."),
                        values.join(", ")
                    ))
                }
                _ => Ok(()),
            }
        }
        Value::Object(filters) => check_facts(filters, path),
        _ => Err(format!(
            "`{}` must be a string, null or the fields of a fact",
            path.join(".")
        )),
    }
}

fn check_definition(definition: &ContextDefinition) -> Checked<()> {
    match definition {
        ContextDefinition::When(when) => check_facts(&when.0, &mut Vec::new()),
        ContextDefinition::Task(filter) if filter.is_empty() => Err(
            "the filter is empty: name a fact (#When) or a task's name, command or labels \
             (#TaskFilter)"
                .to_string(),
        ),
        ContextDefinition::Task(filter) => filter
            .name
            .iter()
            .chain(&filter.command)
            .chain(&filter.labels)
            .try_for_each(|text| pattern(text).map(|_| ())),
    }
}

/// The consumers a context can be active for.
fn consumers(definition: &ContextDefinition) -> Checked<Vec<&'static str>> {
    let when = match definition {
        ContextDefinition::Task(_) => return Ok(vec!["task"]),
        ContextDefinition::When(when) => &when.0,
    };
    let mut possible = CONSUMERS.to_vec();
    if let Some(Value::String(text)) = when.get("consumer") {
        let expression = pattern(text)?;
        possible.retain(|consumer| expression.is_match(consumer));
    }
    // `exec` and `infrastructure` are null for every other consumer.
    for fact in ["exec", "infrastructure"] {
        match when.get(fact) {
            Some(Value::String(_)) => possible.retain(|consumer| *consumer == fact),
            Some(Value::Null) => possible.retain(|consumer| *consumer != fact),
            _ => {}
        }
    }
    Ok(possible)
}

/// Whether no value satisfies both filter strings. `false` means only that
/// cuenv could not prove it.
fn strings_exclusive(first: &str, second: &str, path: &[&str]) -> Checked<bool> {
    let (first_expression, second_expression) = (pattern(first)?, pattern(second)?);
    if let Some(values) = known_values(path) {
        return Ok(!values
            .iter()
            .any(|value| first_expression.is_match(value) && second_expression.is_match(value)));
    }
    Ok(match (is_literal(first), is_literal(second)) {
        (true, _) => !second_expression.is_match(first),
        (_, true) => !first_expression.is_match(second),
        _ => expressions_exclusive(first, second),
    })
}

/// Whether no fact satisfies both filters.
fn fact_filters_exclusive<'filter>(
    first: &'filter Value,
    second: &Value,
    path: &mut Vec<&'filter str>,
) -> Checked<bool> {
    match (first, second) {
        (Value::Null, Value::Null) => Ok(false),
        (Value::Null, _) | (_, Value::Null) => Ok(true),
        (Value::String(first), Value::String(second)) => strings_exclusive(first, second, path),
        (Value::Object(first), Value::Object(second)) => fact_maps_exclusive(first, second, path),
        _ => Ok(false),
    }
}

/// Whether some fact both filters name cannot satisfy both.
fn fact_maps_exclusive<'filter>(
    first: &'filter Map<String, Value>,
    second: &Map<String, Value>,
    path: &mut Vec<&'filter str>,
) -> Checked<bool> {
    for (name, filter) in first {
        let Some(other) = second.get(name) else {
            continue;
        };
        path.push(name);
        let exclusive = fact_filters_exclusive(filter, other, path)?;
        path.pop();
        if exclusive {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Whether two contexts are proved never to be active together. `false` means
/// only that cuenv could not prove it.
fn exclusive(first: &ContextDefinition, second: &ContextDefinition) -> Checked<bool> {
    let others = consumers(second)?;
    if !consumers(first)?
        .iter()
        .any(|consumer| others.contains(consumer))
    {
        return Ok(true);
    }
    match (first, second) {
        (ContextDefinition::When(first), ContextDefinition::When(second)) => {
            fact_maps_exclusive(&first.0, &second.0, &mut Vec::new())
        }
        (ContextDefinition::Task(first), ContextDefinition::Task(second)) => {
            for (first, second) in [
                (&first.name, &second.name),
                (&first.command, &second.command),
            ] {
                if let (Some(first), Some(second)) = (first, second)
                    && strings_exclusive(first, second, &[])?
                {
                    return Ok(true);
                }
            }
            Ok(false)
        }
        _ => Ok(false),
    }
}

/// Check that every context is a valid filter (it names facts an invocation
/// has, and its strings are regular expressions) and that `env.context` can
/// be selected without ambiguity:
///
/// - every context it names is defined;
/// - a variable set under a context is set nowhere else, so it is withheld
///   whenever no context that sets it is active;
/// - two contexts that set the same variable are proved never to be active
///   together.
///
/// # Errors
///
/// Returns a configuration error naming the first violation.
pub fn validate(env: &Env, definitions: &HashMap<String, ContextDefinition>) -> Result<()> {
    let sorted_definitions: BTreeMap<_, _> = definitions.iter().collect();
    for (name, definition) in sorted_definitions {
        check_definition(definition).map_err(|problem| problem_in(name, &problem))?;
    }
    let Some(contexts) = &env.context else {
        return Ok(());
    };
    let contexts: BTreeMap<_, _> = contexts.iter().collect();
    let elsewhere: BTreeSet<&String> = env
        .base
        .keys()
        .chain(env.environment.iter().flatten().flat_map(|(_, v)| v.keys()))
        .collect();
    for (name, context) in &contexts {
        let Some(definition) = definitions.get(*name) else {
            return Err(Error::configuration(format!(
                "env.context.{name} has no matching context: define contexts.{name}"
            )));
        };
        let variables: BTreeSet<&String> = context.variable_names().collect();
        if let Some(variable) = variables
            .iter()
            .find(|variable| elsewhere.contains(*variable))
        {
            return Err(Error::configuration(format!(
                "{variable} is set under env.context.{name} and also outside a context. \
                 A variable set under a context must be set only under contexts, so that \
                 it is withheld when none of them is active"
            )));
        }
        for (other, other_context) in contexts.iter().filter(|(other, _)| *other > name) {
            let Some(variable) = other_context
                .variable_names()
                .find(|variable| variables.contains(variable))
            else {
                continue;
            };
            let Some(other_definition) = definitions.get(*other) else {
                continue;
            };
            if !exclusive(definition, other_definition)
                .map_err(|problem| problem_in(name, &problem))?
            {
                return Err(Error::configuration(format!(
                    "{variable} is set under env.context.{name} and env.context.{other}, \
                     which can both be active for one invocation. Make contexts.{name} and \
                     contexts.{other} exclusive or set the variable under one of them"
                )));
            }
        }
    }
    Ok(())
}

/// A setting outside `env` that names one override per context, such as
/// `infrastructure.state.s3.context`.
#[derive(Debug, Clone, Copy)]
pub struct Overrides<'a> {
    /// Where the overrides are configured, for messages.
    pub field: &'a str,
    /// The contexts that have an override, sorted.
    pub names: &'a [&'a str],
    /// The project's `contexts`.
    pub definitions: &'a HashMap<String, ContextDefinition>,
}

impl<'a> Overrides<'a> {
    /// The context whose override applies to a consumer; `None` when no
    /// context that has one is active.
    ///
    /// # Errors
    ///
    /// Returns a configuration error if an override names an undefined
    /// context, if a filter is invalid, or if two contexts that have an
    /// override are not proved exclusive.
    pub fn active(&self, facts: &Facts, consumer: Consumer<'_>) -> Result<Option<&'a str>> {
        let field = self.field;
        let defined = |name: &str| {
            self.definitions.get(name).ok_or_else(|| {
                Error::configuration(format!(
                    "{field}.{name} has no matching context: define contexts.{name}"
                ))
            })
        };
        for (index, name) in self.names.iter().enumerate() {
            let definition = defined(name)?;
            check_definition(definition).map_err(|problem| problem_in(name, &problem))?;
            for other in &self.names[..index] {
                if !exclusive(defined(other)?, definition)
                    .map_err(|problem| problem_in(name, &problem))?
                {
                    return Err(Error::configuration(format!(
                        "{field}.{other} and {field}.{name} can both be active for one \
                         invocation. Make their filters exclusive"
                    )));
                }
            }
        }
        let document = facts.document(consumer);
        for name in self.names {
            if is_active(defined(name)?, consumer, &document)
                .map_err(|problem| problem_in(name, &problem))?
            {
                return Ok(Some(*name));
            }
        }
        Ok(None)
    }
}

/// A request for the variables one consumer may see.
#[derive(Debug, Clone, Copy)]
pub struct Selection<'a> {
    /// The project's `env`.
    pub env: &'a Env,
    /// The project's `contexts`.
    pub definitions: &'a HashMap<String, ContextDefinition>,
    /// The selected environment (`-e`, `CUENV_ENVIRONMENT`).
    pub environment: Option<&'a str>,
    /// What receives the variables.
    pub consumer: Consumer<'a>,
    /// The project directory, for the git facts.
    pub directory: &'a Path,
}

impl Selection<'_> {
    /// The contexts active for the consumer, sorted. Facts are detected only
    /// when `env.context` sets something; otherwise no context is reported.
    ///
    /// # Errors
    ///
    /// Returns an error if `env.context` is invalid (see [`validate`]).
    pub fn contexts(&self) -> Result<Vec<String>> {
        validate(self.env, self.definitions)?;
        if !self.env.has_context_variables() {
            return Ok(Vec::new());
        }
        self.active(&Facts::detect(self.directory))
    }

    /// As [`Self::contexts`], for facts the caller supplies.
    ///
    /// # Errors
    ///
    /// Returns an error if `env.context` is invalid (see [`validate`]).
    pub fn contexts_with(&self, facts: &Facts) -> Result<Vec<String>> {
        validate(self.env, self.definitions)?;
        self.active(facts)
    }

    fn active(&self, facts: &Facts) -> Result<Vec<String>> {
        let contexts = active_contexts(self.definitions, facts, self.consumer)?;
        tracing::debug!(consumer = ?self.consumer, ?contexts, "Active contexts");
        Ok(contexts)
    }

    /// The variables while `contexts` are active: the base, the selected
    /// environment and each of those contexts.
    #[must_use]
    pub fn variables_in(&self, contexts: &[String]) -> HashMap<String, EnvValue> {
        self.env.select(EnvSelection {
            environment: self.environment,
            contexts,
        })
    }

    /// The variables for the consumer: the base, the selected environment and
    /// every active context.
    ///
    /// # Errors
    ///
    /// Returns an error if `env.context` is invalid (see [`validate`]).
    pub fn variables(&self) -> Result<HashMap<String, EnvValue>> {
        Ok(self.variables_in(&self.contexts()?))
    }

    /// As [`Self::variables`], for facts the caller supplies.
    ///
    /// # Errors
    ///
    /// Returns an error if `env.context` is invalid (see [`validate`]).
    pub fn variables_with(&self, facts: &Facts) -> Result<HashMap<String, EnvValue>> {
        Ok(self.variables_in(&self.contexts_with(facts)?))
    }
}

#[cfg(test)]
#[path = "context_tests.rs"]
mod tests;
