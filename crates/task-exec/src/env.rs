//! Task-level environment variable handling.

use cuenv_core::environment::{EnvValue, Environment, is_withheld};
use cuenv_core::{Error, Result};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap};

/// Resolve task-level environment values into process environment variables.
///
/// Task env values arrive as JSON because they may include runtime-only objects
/// such as output references, host passthrough markers, or secret refs.
///
/// # Errors
///
/// Returns an error when a value that is not a string, number, boolean, or
/// host passthrough is not a valid environment value, or when a secret it
/// references cannot be resolved by its provider.
pub async fn resolve_task_env(
    task_name: &str,
    task_env: &HashMap<String, Value>,
) -> Result<(HashMap<String, String>, Vec<String>)> {
    resolve_task_env_withheld(task_name, task_env, &BTreeSet::new()).await
}

/// Resolve task environment values while excluding reserved host variables
/// from passthrough and secret resolution.
///
/// # Errors
/// Returns an error if a task variable is invalid or a secret cannot resolve.
pub async fn resolve_task_env_withheld(
    task_name: &str,
    task_env: &HashMap<String, Value>,
    withheld_env: &BTreeSet<String>,
) -> Result<(HashMap<String, String>, Vec<String>)> {
    let mut resolved = HashMap::new();
    let mut deferred = HashMap::new();

    for (key, value) in task_env {
        if is_withheld(withheld_env, key) {
            continue;
        }
        if let Some(string_value) = value.as_str() {
            if let Some(host_var) = super::output_refs::parse_passthrough(string_value) {
                if let Some(host_value) = read_passthrough(host_var, withheld_env) {
                    resolved.insert(key.clone(), host_value);
                }
            } else if !string_value.starts_with("cuenv:ref:") {
                resolved.insert(key.clone(), string_value.to_string());
            }
        } else if let Some(number) = value.as_i64() {
            resolved.insert(key.clone(), number.to_string());
        } else if let Some(boolean) = value.as_bool() {
            resolved.insert(key.clone(), boolean.to_string());
        } else if let Some(placeholder) = super::output_refs::try_extract_passthrough(value, key) {
            if let Some(host_var) = super::output_refs::parse_passthrough(&placeholder)
                && let Some(host_value) = read_passthrough(host_var, withheld_env)
            {
                resolved.insert(key.clone(), host_value);
            }
        } else {
            let env_value: EnvValue = serde_json::from_value(value.clone()).map_err(|e| {
                Error::configuration(format!(
                    "Invalid environment value for task '{task_name}' variable '{key}': {e}"
                ))
            })?;
            deferred.insert(key.clone(), env_value);
        }
    }

    if deferred.is_empty() {
        return Ok((resolved, Vec::new()));
    }

    let (deferred_resolved, secrets) =
        Environment::resolve_for_task_withheld(task_name, &deferred, withheld_env).await?;
    resolved.extend(deferred_resolved);

    Ok((resolved, secrets))
}

fn read_passthrough(host_var: &str, withheld_env: &BTreeSet<String>) -> Option<String> {
    if is_withheld(withheld_env, host_var) {
        return None;
    }
    std::env::var(host_var).ok()
}

/// Where the colour of a task's output ends up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorOutput {
    /// cuenv's own output is a terminal, so a child's colour is shown.
    Terminal,
    /// cuenv's output is redirected, or `NO_COLOR` is set. Colour codes would
    /// corrupt what a child prints as data, such as `gh api` writing JSON.
    Plain,
}

impl ColorOutput {
    /// Decide from cuenv's own streams and the host's `NO_COLOR`.
    #[must_use]
    pub fn detect() -> Self {
        use std::io::IsTerminal;
        let no_color = std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty());
        if !no_color && (std::io::stdout().is_terminal() || std::io::stderr().is_terminal()) {
            Self::Terminal
        } else {
            Self::Plain
        }
    }
}

/// Variables that make a child colour its output even though cuenv captures it.
///
/// Empty unless `output` is a terminal, and never replaces a variable the
/// task or the caller already set (`is_set`).
#[must_use]
pub fn forced_color_vars(
    output: ColorOutput,
    is_set: impl Fn(&str) -> bool,
) -> Vec<(&'static str, &'static str)> {
    match output {
        ColorOutput::Plain => Vec::new(),
        ColorOutput::Terminal => ["FORCE_COLOR", "CLICOLOR_FORCE"]
            .into_iter()
            .filter(|name| !is_set(name))
            .map(|name| (name, "1"))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn resolve_task_env_supports_passthrough_and_secret_values() {
        let home = std::env::var("HOME").unwrap();

        let task_env = HashMap::from([
            ("PLAIN".to_string(), json!("literal")),
            (
                "FROM_HOST".to_string(),
                json!({
                    "cuenvPassthrough": true,
                    "name": "HOME"
                }),
            ),
            (
                "SECRET".to_string(),
                json!({
                    "resolver": "exec",
                    "command": "echo",
                    "args": ["resolved-secret"]
                }),
            ),
        ]);

        let (resolved, secrets) = resolve_task_env("publish", &task_env).await.unwrap();
        assert_eq!(resolved.get("PLAIN"), Some(&"literal".to_string()));
        assert_eq!(resolved.get("FROM_HOST"), Some(&home));
        assert_eq!(resolved.get("SECRET"), Some(&"resolved-secret".to_string()));
        assert_eq!(secrets, vec!["resolved-secret".to_string()]);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn task_env_withholds_cache_credentials()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        const CREDENTIAL: &str = "CUENV_TASK_TEST_CACHE_CREDENTIAL";
        const SENTINEL: &str = "private-cache-credential-sentinel";
        if std::env::var(CREDENTIAL).as_deref() != Ok(SENTINEL) {
            let child = std::process::Command::new(std::env::current_exe()?)
                .args([
                    "--exact",
                    "env::tests::task_env_withholds_cache_credentials",
                ])
                .env(CREDENTIAL, SENTINEL)
                .output()?;
            let output = String::from_utf8_lossy(&child.stdout);
            assert!(
                child.status.success() && output.contains("1 passed"),
                "{output}"
            );
            return Ok(());
        }

        let withheld_env = BTreeSet::from([CREDENTIAL.to_owned()]);
        let exec_value = json!({
            "resolver": "exec",
            "command": "sh",
            "args": ["-c", r#"test -z "${CUENV_TASK_TEST_CACHE_CREDENTIAL+x}" && printf resolver-value"#],
        });
        let blocked_value = json!({
            "resolver": "exec",
            "command": "sh",
            "args": ["-c", "exit 17"],
        });
        let task_env = HashMap::from([
            (
                "OBJECT_ALIAS".to_owned(),
                json!({"cuenvPassthrough": true, "name": CREDENTIAL}),
            ),
            (
                "STRING_ALIAS".to_owned(),
                json!(format!("cuenv:passthrough:{CREDENTIAL}")),
            ),
            ("SECRET".to_owned(), exec_value.clone()),
            (CREDENTIAL.to_owned(), blocked_value.clone()),
        ]);
        let (resolved, secrets) =
            resolve_task_env_withheld("test", &task_env, &withheld_env).await?;
        let expected = HashMap::from([("SECRET".to_owned(), "resolver-value".to_owned())]);
        assert_eq!(resolved, expected);
        assert_eq!(secrets, ["resolver-value"]);

        let project_env = HashMap::from([
            ("SECRET".to_owned(), serde_json::from_value(exec_value)?),
            (
                CREDENTIAL.to_owned(),
                serde_json::from_value(blocked_value)?,
            ),
        ]);
        let (resolved, secrets) =
            Environment::resolve_all_withheld(&project_env, &withheld_env).await?;
        assert_eq!(resolved, expected);
        assert_eq!(secrets, ["resolver-value"]);
        assert_eq!(std::env::var(CREDENTIAL)?, SENTINEL);
        Ok(())
    }

    #[test]
    fn forced_color_vars_follow_the_terminal() {
        assert_eq!(forced_color_vars(ColorOutput::Plain, |_| false), Vec::new());
        assert_eq!(
            forced_color_vars(ColorOutput::Terminal, |_| false),
            vec![("FORCE_COLOR", "1"), ("CLICOLOR_FORCE", "1")]
        );
    }

    #[test]
    fn forced_color_vars_keep_what_is_already_set() {
        assert_eq!(
            forced_color_vars(ColorOutput::Terminal, |name| name == "FORCE_COLOR"),
            vec![("CLICOLOR_FORCE", "1")]
        );
    }
}
