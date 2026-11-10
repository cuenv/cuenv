//! The project variables each task of a run receives.
//!
//! A named context can select variables for one task and not for another,
//! and a variable's policies can allow one task and not another, so the
//! tasks of one run do not all share an environment. `cuenv task` and
//! `cuenv ci` both decide it here.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use cuenv_manifest::context::ContextDefinition;
use cuenv_manifest::environment::{Env, EnvSelection, EnvValue};

use super::{Consumer, Facts, TaskSubject, active_contexts, validate};
use crate::Result;

/// The variables selected while one set of contexts is active.
type Selected = Arc<HashMap<String, EnvValue>>;

/// A project's `env` and `contexts`, for the tasks of one run.
#[derive(Debug, Clone)]
pub struct TaskSelectionOptions {
    /// The project's `env`.
    pub env: Env,
    /// The project's `contexts`.
    pub definitions: HashMap<String, ContextDefinition>,
    /// The selected environment (`-e`, `CUENV_ENVIRONMENT`).
    pub environment: Option<String>,
    /// The project directory, for the git facts.
    pub directory: PathBuf,
}

/// What decides the project variables a task receives. Two tasks with the
/// same key receive the same variables.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct TaskEnvKey {
    /// The contexts active for the task, sorted.
    pub contexts: Vec<String>,
    /// The selected variables whose policies allow the task, sorted.
    pub variables: Vec<String>,
}

/// The project variables one task receives.
#[derive(Debug, Clone)]
pub struct TaskEnv {
    key: TaskEnvKey,
    selected: Selected,
}

impl TaskEnv {
    /// What decides these variables.
    #[must_use]
    pub const fn key(&self) -> &TaskEnvKey {
        &self.key
    }

    /// The variables, before they are resolved.
    #[must_use]
    pub fn variables(&self) -> HashMap<String, EnvValue> {
        self.key
            .variables
            .iter()
            .filter_map(|name| Some((name.clone(), self.selected.get(name)?.clone())))
            .collect()
    }
}

/// Selects the project variables of each task of a run. The contexts are
/// validated once, and the variables of a set of contexts are selected once.
#[derive(Debug)]
pub struct TaskSelection {
    options: TaskSelectionOptions,
    selected: Mutex<HashMap<Vec<String>, Selected>>,
}

impl TaskSelection {
    /// The selection for a project.
    ///
    /// # Errors
    ///
    /// Returns an error if `env.context` is invalid (see [`validate`]).
    pub fn new(options: TaskSelectionOptions) -> Result<Self> {
        validate(&options.env, &options.definitions)?;
        Ok(Self {
            options,
            selected: Mutex::default(),
        })
    }

    /// The variables of one task of the graph.
    ///
    /// # Errors
    ///
    /// Returns an error if a filter cannot be read.
    pub fn for_task(&self, task: TaskSubject<'_>) -> Result<TaskEnv> {
        Ok(self.in_contexts(task.name, self.contexts(task)?))
    }

    /// The variables of a name that is not one task, such as a group or a
    /// sequence. No task filter is active for it, whatever its name: its
    /// tasks are matched one at a time.
    ///
    /// # Errors
    ///
    /// Returns an error if a filter cannot be read.
    pub fn for_group(&self, name: &str) -> Result<TaskEnv> {
        let mut contexts = self.contexts(TaskSubject::named(name))?;
        contexts.retain(|context| {
            !matches!(
                self.options.definitions.get(context),
                Some(ContextDefinition::Task(_))
            )
        });
        Ok(self.in_contexts(name, contexts))
    }

    /// The contexts active for a task, sorted. No context is reported, and
    /// no fact detected, unless `env.context` sets something.
    fn contexts(&self, task: TaskSubject<'_>) -> Result<Vec<String>> {
        if !self.options.env.has_context_variables() {
            return Ok(Vec::new());
        }
        active_contexts(
            &self.options.definitions,
            &Facts::detect(&self.options.directory),
            Consumer::Task(task),
        )
    }

    /// The variables of the task called `name` while `contexts` are active.
    fn in_contexts(&self, name: &str, contexts: Vec<String>) -> TaskEnv {
        let selected = self.selected_in(&contexts);
        let mut variables: Vec<String> = selected
            .iter()
            .filter(|(_, value)| value.is_accessible_by_task(name))
            .map(|(variable, _)| variable.clone())
            .collect();
        variables.sort();
        TaskEnv {
            key: TaskEnvKey {
                contexts,
                variables,
            },
            selected,
        }
    }

    /// The variables selected while `contexts` are active, selected once.
    fn selected_in(&self, contexts: &[String]) -> Selected {
        let select = || {
            Arc::new(self.options.env.select(EnvSelection {
                environment: self.options.environment.as_deref(),
                contexts,
            }))
        };
        let Ok(mut known) = self.selected.lock() else {
            return select();
        };
        if let Some(selected) = known.get(contexts) {
            return Arc::clone(selected);
        }
        let selected = select();
        known.insert(contexts.to_vec(), Arc::clone(&selected));
        selected
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_manifest::tasks::Task;
    use serde_json::json;

    fn selection(env: serde_json::Value, contexts: serde_json::Value) -> TaskSelection {
        TaskSelection::new(TaskSelectionOptions {
            env: serde_json::from_value(env).unwrap(),
            definitions: serde_json::from_value(contexts).unwrap(),
            environment: None,
            directory: std::env::temp_dir(),
        })
        .unwrap()
    }

    fn variables_of(selection: &TaskSelection, name: &str) -> Vec<String> {
        let task = Task {
            command: "true".to_string(),
            ..Default::default()
        };
        let env = selection.for_task(TaskSubject::of(name, &task)).unwrap();
        let mut variables: Vec<String> = env.variables().into_keys().collect();
        variables.sort();
        assert_eq!(variables, env.key().variables);
        variables
    }

    #[test]
    fn a_policy_is_checked_for_the_task_that_receives_the_variable() {
        let selection = selection(
            json!({
                "PLAIN": "x",
                "FOR_BUILD": {"value": "b", "policies": [{"allowTasks": ["build"]}]},
                "FOR_DEPLOY": {"value": "d", "policies": [{"allowTasks": ["deploy"]}]},
            }),
            json!({}),
        );
        assert_eq!(variables_of(&selection, "build"), ["FOR_BUILD", "PLAIN"]);
        assert_eq!(variables_of(&selection, "deploy"), ["FOR_DEPLOY", "PLAIN"]);
    }

    #[test]
    fn a_context_variable_reaches_the_task_its_policy_allows() {
        let selection = selection(
            json!({"context": {"build": {
                "TOKEN": {"value": "t", "policies": [{"allowTasks": ["build"]}]},
            }}}),
            json!({"build": {"name": "build"}}),
        );
        assert_eq!(variables_of(&selection, "build"), ["TOKEN"]);
        assert!(variables_of(&selection, "deploy").is_empty());
    }

    #[test]
    fn tasks_with_the_same_contexts_and_policies_share_a_key() {
        let selection = selection(json!({"PLAIN": "x"}), json!({}));
        let task = Task::default();
        let build = selection.for_task(TaskSubject::of("build", &task)).unwrap();
        let test = selection.for_task(TaskSubject::of("test", &task)).unwrap();
        assert_eq!(build.key(), test.key());
        assert!(Arc::ptr_eq(&build.selected, &test.selected));
    }

    #[test]
    fn a_group_is_not_matched_by_a_task_filter() {
        let selection = selection(
            json!({"context": {"deploy": {"HOST": "h"}, "anywhere": {"REGION": "r"}}}),
            json!({"deploy": {"name": "deploy.*"}, "anywhere": {"consumer": "task"}}),
        );
        let group = selection.for_group("deploy").unwrap();
        assert_eq!(group.key().contexts, ["anywhere"]);
        assert_eq!(group.key().variables, ["REGION"]);
        assert_eq!(
            variables_of(&selection, "deploy.staging"),
            ["HOST", "REGION"]
        );
    }
}
