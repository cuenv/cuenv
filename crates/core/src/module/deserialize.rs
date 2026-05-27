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
    message.contains("untagged enum EnvValue") || message.contains("untagged enum EnvValueSimple")
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
