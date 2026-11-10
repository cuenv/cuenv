//! Named contexts: select environment variables from the facts of an invocation.
//!
//! `contexts.<name>.when` is a constraint over a document of facts (`#Context`
//! in `schema/context.cue`). The CUE bridge exports each constraint as a
//! [`ContextMatcher`]; this module builds the document, decides which contexts
//! are active and selects `env.context.<name>` accordingly.
//!
//! Every fact is reported by the machine running cuenv, so a context chooses
//! what an invocation asks for. It never establishes that the invocation is
//! entitled to it: the secrets backend has to enforce that.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};

pub use cuenv_manifest::context::{ContextDefinition, ContextKind, ContextMatcher};
use cuenv_manifest::environment::{Env, EnvSelection, EnvValue};
use cuenv_manifest::manifest::InfrastructurePolicyAction;
use serde_json::{Value, json};

use crate::{Error, Result};

/// What is about to receive the environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consumer<'a> {
    /// A task, by name.
    Task(&'a str),
    /// A command run by `cuenv exec`, by program name.
    Exec(&'a str),
    /// An infrastructure action.
    Infrastructure(InfrastructurePolicyAction),
    /// The shell, or a command that prints the environment.
    Shell,
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
    /// Detect the facts for a project directory. Detection runs `git` and
    /// `hostname`, so the result is kept for the life of the process.
    #[must_use]
    pub fn detect(directory: &Path) -> Arc<Self> {
        static DETECTED: OnceLock<Mutex<HashMap<PathBuf, Arc<Facts>>>> = OnceLock::new();
        let detected = DETECTED.get_or_init(Mutex::default);
        if let Ok(known) = detected.lock()
            && let Some(facts) = known.get(directory)
        {
            return Arc::clone(facts);
        }
        let git = |arguments: &[&str]| {
            command_output(Command::new("git").arg("-C").arg(directory).args(arguments))
        };
        let facts = Arc::new(Self {
            ci: CiFacts::from_variables(|name| std::env::var(name).ok()),
            user_name: git(&["config", "--get", "user.name"]),
            user_email: git(&["config", "--get", "user.email"]),
            host_os: std::env::consts::OS.to_string(),
            host_arch: std::env::consts::ARCH.to_string(),
            host_name: command_output(&mut Command::new("hostname")),
            branch: git(&["symbolic-ref", "--quiet", "--short", "HEAD"]),
        });
        if let Ok(mut known) = detected.lock() {
            known.insert(directory.to_path_buf(), Arc::clone(&facts));
        }
        facts
    }

    /// The `#Context` document for one consumer.
    #[must_use]
    pub fn document(&self, consumer: Consumer<'_>) -> Value {
        let (command, task, exec, infrastructure) = match consumer {
            Consumer::Task(name) => ("task", json!(name), Value::Null, Value::Null),
            Consumer::Exec(program) => ("exec", Value::Null, json!(program), Value::Null),
            Consumer::Infrastructure(action) => {
                ("infrastructure", Value::Null, Value::Null, json!(action))
            }
            Consumer::Shell => ("shell", Value::Null, Value::Null, Value::Null),
        };
        json!({
            "command": command,
            "task": task,
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

/// The trimmed standard output of a command that succeeded with some.
fn command_output(command: &mut Command) -> Option<String> {
    let output = command
        .output()
        .ok()
        .filter(|output| output.status.success())?;
    let text = String::from_utf8(output.stdout).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// Whether a fact satisfies a compiled constraint.
///
/// # Errors
///
/// Returns an error if a regular expression in the constraint is invalid.
pub fn matches(matcher: &ContextMatcher, fact: &Value) -> Result<bool> {
    Ok(match matcher {
        ContextMatcher::Equals(expected) => fact == expected,
        ContextMatcher::NotEquals(excluded) => {
            std::mem::discriminant(fact) == std::mem::discriminant(excluded) && fact != excluded
        }
        ContextMatcher::Kind(kind) => match kind {
            ContextKind::String => fact.is_string(),
            ContextKind::Bool => fact.is_boolean(),
            ContextKind::Number => fact.is_number(),
            ContextKind::Any => true,
        },
        ContextMatcher::Matches(pattern) => match fact.as_str() {
            Some(text) => expression(pattern)?.is_match(text),
            None => false,
        },
        ContextMatcher::NotMatches(pattern) => match fact.as_str() {
            Some(text) => !expression(pattern)?.is_match(text),
            None => false,
        },
        ContextMatcher::AnyOf(matchers) => {
            for matcher in matchers {
                if matches(matcher, fact)? {
                    return Ok(true);
                }
            }
            false
        }
        ContextMatcher::AllOf(matchers) => {
            for matcher in matchers {
                if !matches(matcher, fact)? {
                    return Ok(false);
                }
            }
            true
        }
        ContextMatcher::Fields(fields) => {
            let Some(object) = fact.as_object() else {
                return Ok(false);
            };
            for (name, matcher) in fields {
                let Some(field) = object.get(name) else {
                    return Ok(false);
                };
                if !matches(matcher, field)? {
                    return Ok(false);
                }
            }
            true
        }
    })
}

fn expression(pattern: &str) -> Result<regex::Regex> {
    regex::Regex::new(pattern).map_err(|error| {
        Error::configuration(format!(
            "context constraint has an invalid regular expression `{pattern}`: {error}"
        ))
    })
}

/// The names of the contexts a document satisfies, sorted.
///
/// # Errors
///
/// Returns an error if a constraint holds an invalid regular expression.
pub fn active_contexts(
    definitions: &HashMap<String, ContextDefinition>,
    document: &Value,
) -> Result<Vec<String>> {
    let mut active = Vec::new();
    for (name, definition) in definitions {
        if matches(&definition.when, document)? {
            active.push(name.clone());
        }
    }
    active.sort();
    Ok(active)
}

/// The fields of the `#Context` document at a path, or `None` for a fact that
/// has no fields.
fn document_fields(path: &[&str]) -> Option<&'static [&'static str]> {
    match path {
        [] => Some(&[
            "command",
            "task",
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

/// Reject a constraint over a fact the document does not have. CUE does not
/// do this: a project embeds its schema at the top of the file, which leaves
/// `when` open, so a misspelt fact would otherwise be a context that never
/// matches.
fn check_facts<'matcher>(
    matcher: &'matcher ContextMatcher,
    path: &mut Vec<&'matcher str>,
) -> std::result::Result<(), String> {
    match matcher {
        ContextMatcher::AnyOf(matchers) | ContextMatcher::AllOf(matchers) => matchers
            .iter()
            .try_for_each(|matcher| check_facts(matcher, path)),
        ContextMatcher::Fields(fields) => {
            let known = document_fields(path).unwrap_or_default();
            for (name, matcher) in fields {
                path.push(name);
                if !known.contains(&name.as_str()) {
                    return Err(format!(
                        "`{}` is not a fact of an invocation",
                        path.join(".")
                    ));
                }
                check_facts(matcher, path)?;
                path.pop();
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Check that `env.context` can be selected without ambiguity:
///
/// - every constraint is over facts the document has;
/// - every context it names is defined;
/// - a variable set under a context is set nowhere else, so it is withheld
///   whenever no context that sets it is active;
/// - two contexts that can be active together do not set the same variable.
///
/// # Errors
///
/// Returns a configuration error naming the first violation.
pub fn validate(env: &Env, definitions: &HashMap<String, ContextDefinition>) -> Result<()> {
    let Some(contexts) = &env.context else {
        return Ok(());
    };
    let sorted_definitions: BTreeMap<_, _> = definitions.iter().collect();
    for (name, definition) in sorted_definitions {
        check_facts(&definition.when, &mut Vec::new())
            .map_err(|problem| Error::configuration(format!("contexts.{name}.when: {problem}")))?;
    }
    let contexts: BTreeMap<_, _> = contexts.iter().collect();
    let elsewhere: BTreeSet<&String> = env
        .base
        .keys()
        .chain(env.environment.iter().flatten().flat_map(|(_, v)| v.keys()))
        .collect();
    for (name, context) in &contexts {
        let Some(definition) = definitions.get(*name) else {
            return Err(Error::configuration(format!(
                "env.context.{name} has no matching context: define contexts.{name}.when"
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
        for other in definition.overlaps.iter().filter(|other| *other > *name) {
            let Some(other_context) = contexts.get(other) else {
                continue;
            };
            if let Some(variable) = other_context
                .variable_names()
                .find(|variable| variables.contains(variable))
            {
                return Err(Error::configuration(format!(
                    "{variable} is set under env.context.{name} and env.context.{other}, \
                     which can both be active for one invocation. Make their `when` \
                     constraints exclusive or set the variable under one of them"
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
    /// The context whose override applies to a document; `None` when no
    /// context that has one is active.
    ///
    /// # Errors
    ///
    /// Returns a configuration error if an override names an undefined
    /// context, if a constraint is over an unknown fact, or if two contexts
    /// that have an override can be active together.
    pub fn active(&self, document: &Value) -> Result<Option<&'a str>> {
        let field = self.field;
        for name in self.names {
            let Some(definition) = self.definitions.get(*name) else {
                return Err(Error::configuration(format!(
                    "{field}.{name} has no matching context: define contexts.{name}.when"
                )));
            };
            check_facts(&definition.when, &mut Vec::new()).map_err(|problem| {
                Error::configuration(format!("contexts.{name}.when: {problem}"))
            })?;
            if let Some(other) = definition
                .overlaps
                .iter()
                .find(|other| other.as_str() > *name && self.names.contains(&other.as_str()))
            {
                return Err(Error::configuration(format!(
                    "{field}.{name} and {field}.{other} can both be active for one \
                     invocation. Make their `when` constraints exclusive"
                )));
            }
        }
        let mut active = None;
        for name in self.names {
            if matches(&self.definitions[*name].when, document)? {
                if let Some(first) = active {
                    return Err(Error::configuration(format!(
                        "{field}.{first} and {field}.{name} are both active"
                    )));
                }
                active = Some(*name);
            }
        }
        Ok(active)
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
    /// The variables for the consumer: the base, the selected environment and
    /// every active context. Facts are detected only when `env.context` sets
    /// something.
    ///
    /// # Errors
    ///
    /// Returns an error if `env.context` is invalid (see [`validate`]).
    pub fn variables(&self) -> Result<HashMap<String, EnvValue>> {
        if !self.env.has_context_variables() {
            return Ok(self.env.select(EnvSelection {
                environment: self.environment,
                contexts: &[],
            }));
        }
        self.variables_with(&Facts::detect(self.directory))
    }

    /// As [`Self::variables`], for facts the caller supplies.
    ///
    /// # Errors
    ///
    /// Returns an error if `env.context` is invalid (see [`validate`]).
    pub fn variables_with(&self, facts: &Facts) -> Result<HashMap<String, EnvValue>> {
        validate(self.env, self.definitions)?;
        let document = facts.document(self.consumer);
        let contexts = active_contexts(self.definitions, &document)?;
        tracing::debug!(consumer = ?self.consumer, ?contexts, "Active contexts");
        Ok(self.env.select(EnvSelection {
            environment: self.environment,
            contexts: &contexts,
        }))
    }
}

#[cfg(test)]
#[path = "context_tests.rs"]
mod tests;
