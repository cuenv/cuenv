//! Task environment precedence helpers for CI execution.

use std::collections::BTreeMap;

/// Apply task-level environment variables to the merged task environment.
///
/// Task env has the highest precedence. Passthrough placeholders intentionally
/// read from the host process at execution time so CI-provided values such as
/// GitHub Actions context variables remain available inside hermetic tasks.
pub(super) fn apply_task_env(
    env: &mut BTreeMap<String, String>,
    task_env: &BTreeMap<String, String>,
) {
    for (key, value) in task_env {
        if let Some(host_var) = cuenv_core::tasks::output_refs::parse_passthrough(value) {
            match std::env::var(host_var) {
                Ok(host_value) => {
                    env.insert(key.clone(), host_value);
                }
                Err(_) => {
                    env.remove(key);
                }
            }
        } else if !value.starts_with("cuenv:ref:") {
            env.insert(key.clone(), value.clone());
        }
    }
}

pub(super) fn resolve_environment(
    cli_environment: Option<&str>,
    pipeline_environment: Option<&str>,
) -> Option<String> {
    if let Some(env) = cli_environment.filter(|name| !name.is_empty()) {
        return Some(env.to_string());
    }

    if let Ok(env) = std::env::var("CUENV_ENVIRONMENT")
        && !env.is_empty()
    {
        return Some(env);
    }

    pipeline_environment
        .filter(|name| !name.is_empty())
        .map(|name| name.to_string())
}

#[cfg(test)]
mod tests {
    use super::apply_task_env;
    use std::collections::BTreeMap;

    #[test]
    fn task_env_passthrough_reads_host_environment() {
        temp_env::with_var("CUENV_TEST_GITHUB_ACTOR", Some("octocat"), || {
            let mut env = BTreeMap::from([("GITHUB_ACTOR".to_string(), "lower".to_string())]);
            let task_env = BTreeMap::from([(
                "GITHUB_ACTOR".to_string(),
                "cuenv:passthrough:CUENV_TEST_GITHUB_ACTOR".to_string(),
            )]);

            apply_task_env(&mut env, &task_env);

            assert_eq!(env.get("GITHUB_ACTOR"), Some(&"octocat".to_string()));
        });
    }

    #[test]
    fn missing_task_env_passthrough_unsets_lower_precedence_value() {
        temp_env::with_var_unset("CUENV_TEST_MISSING_ACTOR", || {
            let mut env = BTreeMap::from([("GITHUB_ACTOR".to_string(), "lower".to_string())]);
            let task_env = BTreeMap::from([(
                "GITHUB_ACTOR".to_string(),
                "cuenv:passthrough:CUENV_TEST_MISSING_ACTOR".to_string(),
            )]);

            apply_task_env(&mut env, &task_env);

            assert!(!env.contains_key("GITHUB_ACTOR"));
        });
    }

    #[test]
    fn literal_task_env_overrides_lower_precedence_value() {
        let mut env = BTreeMap::from([("GITHUB_REF_NAME".to_string(), "main".to_string())]);
        let task_env = BTreeMap::from([("GITHUB_REF_NAME".to_string(), "release".to_string())]);

        apply_task_env(&mut env, &task_env);

        assert_eq!(env.get("GITHUB_REF_NAME"), Some(&"release".to_string()));
    }
}
