//! How a run names its project and environment on a command line.
//!
//! Every command the tool prints as a hint (`unlock`, `state recover`,
//! `state adopt`, `plan`) must act on the state the failed run acted on, so
//! each carries the `--env` the run selected and the `-p` and `--package`
//! it was given when they differ from the defaults. Without them a hint run
//! verbatim would look at another environment's state, or at none.

use cuenv_infrastructure::TenantLock;

use super::evaluation::escape_control_characters;
use super::{InfrastructureAction, InfrastructureOptions, UnlockScope};

/// Default value of `-p` / `--path`.
const DEFAULT_PATH: &str = ".";

/// Default value of `--package`.
const DEFAULT_PACKAGE: &str = "cuenv";

/// The project and environment a run selected, as command-line flags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Invocation {
    environment: Option<String>,
    path: String,
    package: String,
    /// The project an `unlock` run named with `--module` and `--project`:
    /// the `unlock` commands of its hints name it again.
    unlock_scope: UnlockScope,
}

impl Default for Invocation {
    fn default() -> Self {
        Self {
            environment: None,
            path: DEFAULT_PATH.to_string(),
            package: DEFAULT_PACKAGE.to_string(),
            unlock_scope: UnlockScope::default(),
        }
    }
}

impl Invocation {
    /// The invocation a run's options describe.
    #[must_use]
    pub(super) fn of(options: &InfrastructureOptions) -> Self {
        Self {
            environment: options.environment.clone(),
            path: options.path.clone(),
            package: options.package.clone(),
            unlock_scope: match &options.action {
                InfrastructureAction::Unlock { scope, .. } => scope.clone(),
                _ => UnlockScope::default(),
            },
        }
    }

    /// The same project and options with another environment (`None`: no
    /// `--env`).
    #[must_use]
    pub(super) fn with_environment(&self, environment: Option<&str>) -> Self {
        Self {
            environment: environment.map(str::to_owned),
            ..self.clone()
        }
    }

    /// `cuenv infrastructure <subcommand>` with the flags that select this
    /// run's environment and project, for a hint to print.
    #[must_use]
    pub(super) fn command(&self, subcommand: &str) -> String {
        let mut words = vec![format!("cuenv infrastructure {subcommand}")];
        if subcommand == "unlock" || subcommand.starts_with("unlock ") {
            if let Some(module_path) = &self.unlock_scope.module_path {
                words.push(format!("--module {}", quote_argument(module_path)));
            }
            if let Some(project) = &self.unlock_scope.project {
                words.push(format!("--project {}", quote_argument(project)));
            }
        }
        if let Some(environment) = &self.environment {
            words.push(format!("--env {}", quote_argument(environment)));
        }
        if self.path != DEFAULT_PATH {
            words.push(format!("-p {}", quote_argument(&self.path)));
        }
        if self.package != DEFAULT_PACKAGE {
            words.push(format!("--package {}", quote_argument(&self.package)));
        }
        words.join(" ")
    }
}

impl Invocation {
    /// The command that releases `lock`, whichever project holds it: it names
    /// the lock's own module, project and environment, and carries this run's
    /// `-p` and `--package`, because those only select the project whose
    /// `infrastructure.state` says which database to reach.
    #[must_use]
    pub(super) fn unlock_command(&self, lock: &TenantLock) -> String {
        Self {
            environment: lock.environment.clone(),
            unlock_scope: UnlockScope::default(),
            ..self.clone()
        }
        .command(&format!(
            "unlock {} --module {} --project {}",
            quote_argument(&lock.lock.lock_identifier),
            quote_argument(&lock.module_path),
            quote_argument(&lock.project)
        ))
    }
}

/// An argument as a shell would take it back: bare when it is made of safe
/// characters, otherwise in single quotes, with control characters shown
/// escaped (never raw: a hint must not drive the terminal).
pub(super) fn quote_argument(value: &str) -> String {
    let safe = !value.is_empty()
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-' | '/' | '@')
        });
    if safe {
        return value.to_string();
    }
    format!(
        "'{}'",
        escape_control_characters(value).replace('\'', "'\\''")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invocation(environment: Option<&str>, path: &str, package: &str) -> Invocation {
        Invocation {
            environment: environment.map(str::to_owned),
            path: path.to_string(),
            package: package.to_string(),
            unlock_scope: UnlockScope::default(),
        }
    }

    #[test]
    fn a_named_project_is_named_again_by_the_unlock_hints_only() {
        let named = Invocation {
            unlock_scope: UnlockScope {
                module_path: Some("example.com/gone".to_string()),
                project: Some("ghost".to_string()),
            },
            ..invocation(Some("Prod"), ".", "cuenv")
        };
        assert_eq!(
            named.command("unlock lock-1"),
            "cuenv infrastructure unlock lock-1 --module example.com/gone --project ghost --env Prod"
        );
        assert_eq!(
            named.command("state locks"),
            "cuenv infrastructure state locks --env Prod"
        );
        // A hint for a listed lock names that lock's own project, once.
        let lock = TenantLock {
            module_path: "example.com/other".to_string(),
            project: "api".to_string(),
            environment: None,
            lock: cuenv_infrastructure::LockInformation {
                lock_identifier: "lock-2".to_string(),
                holder: "ci".to_string(),
                acquired_at: "2026-01-01T00:00:00+00:00".to_string(),
            },
        };
        assert_eq!(
            named.unlock_command(&lock),
            "cuenv infrastructure unlock lock-2 --module example.com/other --project api"
        );
    }

    #[test]
    fn defaults_add_nothing() {
        assert_eq!(
            Invocation::default().command("state recover"),
            "cuenv infrastructure state recover"
        );
    }

    #[test]
    fn the_environment_and_non_default_project_selectors_are_carried() {
        assert_eq!(
            invocation(Some("dev"), "./services/api", "ops").command("unlock abc"),
            "cuenv infrastructure unlock abc --env dev -p ./services/api --package ops"
        );
        assert_eq!(
            invocation(None, ".", "cuenv")
                .with_environment(Some("prod"))
                .command("state adopt"),
            "cuenv infrastructure state adopt --env prod"
        );
    }

    #[test]
    fn unusual_values_are_quoted_and_never_printed_raw() {
        let command = invocation(Some("a b\u{1b}[31m'x"), ".", "cuenv").command("plan");
        assert_eq!(
            command,
            "cuenv infrastructure plan --env 'a b\\u{1b}[31m'\\''x'"
        );
        assert!(!command.contains('\u{1b}'));
    }
}
