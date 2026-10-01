//! How a run names its project and environment on a command line.
//!
//! Every command the tool prints as a hint (`unlock`, `state recover`,
//! `state adopt`, `plan`) must act on the state the failed run acted on, so
//! each carries the `--env` the run selected and the `-p` and `--package`
//! it was given when they differ from the defaults. Without them a hint run
//! verbatim would look at another environment's state, or at none.

use super::InfrastructureOptions;
use super::evaluation::escape_control_characters;

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
}

impl Default for Invocation {
    fn default() -> Self {
        Self {
            environment: None,
            path: DEFAULT_PATH.to_string(),
            package: DEFAULT_PACKAGE.to_string(),
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
        let mut command = format!("cuenv infrastructure {subcommand}");
        if let Some(environment) = &self.environment {
            command.push_str(&format!(" --env {}", quote_argument(environment)));
        }
        if self.path != DEFAULT_PATH {
            command.push_str(&format!(" -p {}", quote_argument(&self.path)));
        }
        if self.package != DEFAULT_PACKAGE {
            command.push_str(&format!(" --package {}", quote_argument(&self.package)));
        }
        command
    }
}

/// An argument as a shell would take it back: bare when it is made of safe
/// characters, otherwise in single quotes, with control characters shown
/// escaped (never raw: a hint must not drive the terminal).
fn quote_argument(value: &str) -> String {
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
        }
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
