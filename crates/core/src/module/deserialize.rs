use serde::de::DeserializeOwned;

const ENV_VALUE_HINT: &str = "Hint: `env` values must be a string, int, bool, secret object (`{resolver: ...}`), interpolated array (`[\"prefix\", {resolver: ...}]`), or `{ value: <value>, policies: [...] }`.";

pub(super) fn detailed_deserialize_error<T: DeserializeOwned>(
    value: &serde_json::Value,
    fallback: &serde_json::Error,
) -> String {
    let json = value.to_string();
    let mut deserializer = serde_json::Deserializer::from_str(&json);
    match serde_path_to_error::deserialize::<_, T>(&mut deserializer) {
        Ok(_) => fallback.to_string(),
        Err(error) => {
            let path = error.path().to_string();
            let inner_message = error.into_inner().to_string();
            let mut display_path = if path.is_empty() { None } else { Some(path) };

            if should_include_env_value_hint(&inner_message)
                && let Some(env_path) = find_invalid_env_value_path(value)
            {
                display_path = Some(env_path);
            }

            let mut message = match display_path {
                Some(path) => format!("{inner_message} (at `{path}`)"),
                None => inner_message,
            };

            if should_include_env_value_hint(&message) {
                message.push_str(". ");
                message.push_str(ENV_VALUE_HINT);
            }

            message
        }
    }
}

fn should_include_env_value_hint(message: &str) -> bool {
    message.contains("untagged enum EnvValue")
        || message.contains("untagged enum EnvValueSimple")
        || message.contains("environment variable")
}

fn find_invalid_env_value_path(value: &serde_json::Value) -> Option<String> {
    let env = value.get("env")?.as_object()?;

    for (key, raw_value) in env {
        if key == "environment" {
            continue;
        }

        if serde_json::from_value::<crate::environment::EnvValue>(raw_value.clone()).is_err() {
            return Some(format!("env.{key}"));
        }
    }

    let environments = env.get("environment")?.as_object()?;
    for (environment_name, overrides) in environments {
        let overrides = overrides.as_object()?;
        for (key, raw_value) in overrides {
            if serde_json::from_value::<crate::environment::EnvValue>(raw_value.clone()).is_err() {
                return Some(format!("env.environment.{environment_name}.{key}"));
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Project;
    use serde_json::json;

    fn error_for(env: serde_json::Value) -> String {
        let value = json!({"name": "api", "env": env});
        let fallback = serde_json::from_str::<Project>("{").unwrap_err();
        detailed_deserialize_error::<Project>(&value, &fallback)
    }

    fn assert_names_variable(message: &str, path: &str) {
        assert!(message.contains(&format!("(at `{path}`)")), "{message}");
        assert!(message.contains(ENV_VALUE_HINT), "{message}");
    }

    #[test]
    fn invalid_interpolated_value_keeps_its_variable_path_and_hint() {
        let message = error_for(json!({"A": ["a", {"command": "echo"}]}));
        assert_names_variable(&message, "env.A");
    }

    #[test]
    fn invalid_interpolated_value_in_an_environment_override_keeps_its_path() {
        let message = error_for(json!({"environment": {"dev": {"A": ["a", {"command": "echo"}]}}}));
        assert_names_variable(&message, "env.environment.dev.A");
    }

    #[test]
    fn project_passthrough_is_explained_not_reported_as_a_secret() {
        let message = error_for(json!({"A": {"cuenvPassthrough": true, "name": "USER"}}));
        assert_names_variable(&message, "env.A");
        assert!(message.contains("task's `env`"), "{message}");
        assert!(!message.contains("missing field"), "{message}");
    }

    #[test]
    fn null_value_inside_a_policies_object_is_called_incomplete() {
        let message = error_for(json!({"A": {"value": null, "policies": []}}));
        assert_names_variable(&message, "env.A");
        assert!(message.contains("incomplete CUE value"), "{message}");
    }
}
