//! Infisical command-layer preprocessing.
//!
//! Infisical secrets need runtime context (selected `-e`, config defaults, and
//! instance filesystem path) before resolver execution. This module fills missing
//! `environment`, `projectId`, and normalizes `path` for `resolver: "infisical"`.

use cuenv_core::config::InfisicalConfig;
use cuenv_core::environment::{EnvPart, EnvValue, EnvValueSimple};
use cuenv_core::module::FieldMeta;
use cuenv_core::secrets::Secret;
use cuenv_core::{Error, ModuleEvaluation, Result};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Context used to normalize Infisical secrets before resolution.
pub struct InfisicalPreprocessContext<'a> {
    /// Environment selected via CLI `-e`, if present.
    pub selected_environment: Option<&'a str>,
    /// Infisical defaults from `config.infisical`.
    pub config: Option<&'a InfisicalConfig>,
    /// Absolute filesystem path of the evaluated instance.
    pub instance_path: &'a Path,
    /// Optional env var -> defining directory mapping derived from CUE metadata.
    pub env_var_definition_dirs: Option<&'a HashMap<String, PathBuf>>,
}

impl InfisicalPreprocessContext<'_> {
    fn definition_dir_for(&self, env_key: &str) -> Option<&Path> {
        self.env_var_definition_dirs
            .and_then(|map| map.get(env_key))
            .map(PathBuf::as_path)
    }
}

/// Resolve source directories for env keys from CUE field metadata.
///
/// This follows CUE references, so if an environment value is defined via
/// reference to another field, the referenced field location is used.
#[must_use]
pub fn resolve_env_definition_dirs(
    module: &ModuleEvaluation,
    instance_path: &Path,
    selected_environment: Option<&str>,
    env_keys: &[String],
) -> HashMap<String, PathBuf> {
    let mut result = HashMap::new();

    for env_key in env_keys {
        let mut candidates = Vec::with_capacity(2);
        if let Some(environment) = selected_environment
            && !environment.is_empty()
        {
            candidates.push(format!("env.environment.{environment}.{env_key}"));
        }
        candidates.push(format!("env.{env_key}"));

        for candidate in candidates {
            if let Some(meta) = module.resolved_field_meta(instance_path, &candidate) {
                result.insert(env_key.clone(), definition_dir_from_meta(module, meta));
                break;
            }
        }
    }

    result
}

fn definition_dir_from_meta(module: &ModuleEvaluation, meta: &FieldMeta) -> PathBuf {
    if !meta.filename.trim().is_empty() {
        let filename = Path::new(&meta.filename);
        let parent = filename.parent().unwrap_or_else(|| Path::new(""));

        if filename.is_absolute() {
            return parent.to_path_buf();
        }
        return module.root.join(parent);
    }

    if !meta.directory.trim().is_empty() {
        let dir = Path::new(&meta.directory);
        if dir.is_absolute() {
            return dir.to_path_buf();
        }
        return module.root.join(dir);
    }

    module.root.clone()
}

/// Normalize all Infisical secrets in a map of environment variables.
///
/// This mutates only secrets with `resolver: "infisical"`.
///
/// # Errors
///
/// Returns a configuration error when required Infisical fields cannot be derived.
pub fn preprocess_infisical_secrets(
    env_vars: &mut HashMap<String, EnvValue>,
    context: &InfisicalPreprocessContext<'_>,
) -> Result<()> {
    for (env_key, value) in env_vars {
        preprocess_env_value(env_key, value, context)?;
    }
    Ok(())
}

fn preprocess_env_value(
    env_key: &str,
    value: &mut EnvValue,
    context: &InfisicalPreprocessContext<'_>,
) -> Result<()> {
    match value {
        EnvValue::Secret(secret) => preprocess_secret(env_key, env_key, secret, context),
        EnvValue::Interpolated(parts) => preprocess_parts(env_key, parts, context),
        EnvValue::WithPolicies(var) => preprocess_simple_value(env_key, &mut var.value, context),
        EnvValue::String(_) | EnvValue::Int(_) | EnvValue::Bool(_) => Ok(()),
    }
}

fn preprocess_simple_value(
    env_key: &str,
    value: &mut EnvValueSimple,
    context: &InfisicalPreprocessContext<'_>,
) -> Result<()> {
    match value {
        EnvValueSimple::Secret(secret) => preprocess_secret(env_key, env_key, secret, context),
        EnvValueSimple::Interpolated(parts) => preprocess_parts(env_key, parts, context),
        EnvValueSimple::String(_) | EnvValueSimple::Int(_) | EnvValueSimple::Bool(_) => Ok(()),
    }
}

fn preprocess_parts(
    env_key: &str,
    parts: &mut [EnvPart],
    context: &InfisicalPreprocessContext<'_>,
) -> Result<()> {
    for (index, part) in parts.iter_mut().enumerate() {
        if let EnvPart::Secret(secret) = part {
            let scoped_key = format!("{env_key}[{index}]");
            preprocess_secret(&scoped_key, env_key, secret, context)?;
        }
    }
    Ok(())
}

fn preprocess_secret(
    display_key: &str,
    lookup_key: &str,
    secret: &mut Secret,
    context: &InfisicalPreprocessContext<'_>,
) -> Result<()> {
    if secret.resolver != "infisical" {
        return Ok(());
    }

    let raw_path = string_field(secret, display_key, "path")?.ok_or_else(|| {
        Error::configuration(format!(
            "Environment variable '{display_key}' uses resolver 'infisical' but is missing required field 'path'"
        ))
    })?;
    let normalized_path = resolve_path(display_key, lookup_key, &raw_path, context)?;

    let environment = resolve_environment(display_key, secret, context)?;
    let project_id = resolve_project_id(display_key, secret, context)?;

    secret
        .extra
        .insert("path".to_string(), Value::String(normalized_path));
    secret
        .extra
        .insert("environment".to_string(), Value::String(environment));
    secret
        .extra
        .insert("projectId".to_string(), Value::String(project_id));

    Ok(())
}

fn resolve_path(
    env_key: &str,
    lookup_key: &str,
    raw_path: &str,
    context: &InfisicalPreprocessContext<'_>,
) -> Result<String> {
    let trimmed = raw_path.trim();
    if trimmed.is_empty() {
        return Err(Error::configuration(format!(
            "Environment variable '{env_key}' has empty infisical 'path'"
        )));
    }

    let mut resolved = if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        let inherit_path = context
            .config
            .and_then(|cfg| cfg.inherit_path)
            .unwrap_or(false);

        if !inherit_path {
            return Err(Error::configuration(format!(
                "Environment variable '{env_key}' uses relative infisical path '{trimmed}' but config.infisical.inheritPath is not enabled"
            )));
        }

        let base_dir = context
            .definition_dir_for(lookup_key)
            .unwrap_or(context.instance_path);
        base_dir.join(trimmed).to_string_lossy().into_owned()
    };

    if let Some(path_replace) = context.config.and_then(|cfg| cfg.path_replace.as_ref()) {
        let mut keys: Vec<&String> = path_replace.keys().collect();
        keys.sort();

        for key in keys {
            if key.is_empty() {
                return Err(Error::configuration(
                    "config.infisical.pathReplace cannot contain an empty key".to_string(),
                ));
            }

            if let Some(value) = path_replace.get(key) {
                resolved = resolved.replace(key, value);
            }
        }
    }

    resolved = resolved.replace('\\', "/");
    if !resolved.starts_with('/') {
        resolved.insert(0, '/');
    }
    if resolved.len() > 1 {
        resolved = resolved.trim_end_matches('/').to_string();
    }

    Ok(resolved)
}

fn resolve_environment(
    env_key: &str,
    secret: &Secret,
    context: &InfisicalPreprocessContext<'_>,
) -> Result<String> {
    if let Some(value) = string_field(secret, env_key, "environment")? {
        if value.trim().is_empty() {
            return Err(Error::configuration(format!(
                "Environment variable '{env_key}' has empty infisical 'environment'"
            )));
        }
        return Ok(value);
    }

    if let Some(selected) = context.selected_environment {
        let selected = selected.trim();
        if !selected.is_empty() {
            return Ok(selected.to_string());
        }
    }

    if let Some(default_environment) = context
        .config
        .and_then(|cfg| cfg.default_environment.as_deref())
    {
        let default_environment = default_environment.trim();
        if !default_environment.is_empty() {
            return Ok(default_environment.to_string());
        }
    }

    Err(Error::configuration(format!(
        "Environment variable '{env_key}' uses resolver 'infisical' but no environment is set. \
Set one of: secret.environment, CLI -e, or config.infisical.defaultEnvironment."
    )))
}

fn resolve_project_id(
    env_key: &str,
    secret: &Secret,
    context: &InfisicalPreprocessContext<'_>,
) -> Result<String> {
    if let Some(value) = string_field(secret, env_key, "projectId")? {
        if value.trim().is_empty() {
            return Err(Error::configuration(format!(
                "Environment variable '{env_key}' has empty infisical 'projectId'"
            )));
        }
        return Ok(value);
    }

    if let Some(default_project_id) = context.config.and_then(|cfg| cfg.project_id.as_deref()) {
        let default_project_id = default_project_id.trim();
        if !default_project_id.is_empty() {
            return Ok(default_project_id.to_string());
        }
    }

    Err(Error::configuration(format!(
        "Environment variable '{env_key}' uses resolver 'infisical' but no projectId is set. \
Set one of: secret.projectId or config.infisical.projectId."
    )))
}

fn string_field(secret: &Secret, env_key: &str, field: &str) -> Result<Option<String>> {
    let Some(value) = secret.extra.get(field) else {
        return Ok(None);
    };

    match value {
        Value::String(s) => Ok(Some(s.clone())),
        _ => Err(Error::configuration(format!(
            "Environment variable '{env_key}' field '{field}' must be a string"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_core::environment::EnvValue;
    use cuenv_core::module::FieldMeta;
    use serde_json::json;

    fn infisical_secret(path: &str) -> Secret {
        let mut secret = Secret {
            resolver: "infisical".to_string(),
            command: String::new(),
            args: Vec::new(),
            op_ref: None,
            extra: HashMap::new(),
        };
        secret
            .extra
            .insert("path".to_string(), Value::String(path.to_string()));
        secret
    }

    #[test]
    fn applies_cli_environment_and_config_project() {
        let mut env = HashMap::new();
        env.insert(
            "API_KEY".to_string(),
            EnvValue::Secret(infisical_secret("/team/app/API_KEY")),
        );

        let cfg = InfisicalConfig {
            project_id: Some("proj_123".to_string()),
            ..Default::default()
        };
        let ctx = InfisicalPreprocessContext {
            selected_environment: Some("production"),
            config: Some(&cfg),
            instance_path: Path::new("/workspace/service"),
            env_var_definition_dirs: None,
        };

        preprocess_infisical_secrets(&mut env, &ctx).unwrap();

        let EnvValue::Secret(secret) = env.get("API_KEY").unwrap() else {
            panic!("expected secret");
        };
        assert_eq!(
            secret.extra.get("environment"),
            Some(&Value::String("production".to_string()))
        );
        assert_eq!(
            secret.extra.get("projectId"),
            Some(&Value::String("proj_123".to_string()))
        );
    }

    #[test]
    fn resolves_relative_path_when_inherit_enabled() {
        let mut env = HashMap::new();
        env.insert(
            "API_KEY".to_string(),
            EnvValue::Secret(infisical_secret("API_KEY")),
        );

        let mut replace = HashMap::new();
        replace.insert(".".to_string(), "-".to_string());
        let cfg = InfisicalConfig {
            inherit_path: Some(true),
            path_replace: Some(replace),
            default_environment: Some("development".to_string()),
            project_id: Some("proj_123".to_string()),
        };
        let ctx = InfisicalPreprocessContext {
            selected_environment: None,
            config: Some(&cfg),
            instance_path: Path::new("/repo/rawkode.cloud"),
            env_var_definition_dirs: None,
        };

        preprocess_infisical_secrets(&mut env, &ctx).unwrap();

        let EnvValue::Secret(secret) = env.get("API_KEY").unwrap() else {
            panic!("expected secret");
        };
        assert_eq!(
            secret.extra.get("path"),
            Some(&Value::String("/repo/rawkode-cloud/API_KEY".to_string()))
        );
    }

    #[test]
    fn resolves_relative_path_from_definition_directory_not_instance_path() {
        let mut env = HashMap::new();
        env.insert(
            "A_KEY".to_string(),
            EnvValue::Secret(infisical_secret("A_KEY")),
        );

        let cfg = InfisicalConfig {
            inherit_path: Some(true),
            default_environment: Some("development".to_string()),
            project_id: Some("proj_123".to_string()),
            ..Default::default()
        };
        let mut definition_dirs = HashMap::new();
        definition_dirs.insert("A_KEY".to_string(), PathBuf::from("/repo/shared"));

        let ctx = InfisicalPreprocessContext {
            selected_environment: None,
            config: Some(&cfg),
            instance_path: Path::new("/repo/apps/current"),
            env_var_definition_dirs: Some(&definition_dirs),
        };

        preprocess_infisical_secrets(&mut env, &ctx).unwrap();

        let EnvValue::Secret(secret) = env.get("A_KEY").unwrap() else {
            panic!("expected secret");
        };
        assert_eq!(
            secret.extra.get("path"),
            Some(&Value::String("/repo/shared/A_KEY".to_string()))
        );
    }

    #[test]
    fn resolves_definition_directory_from_cue_reference_metadata() {
        let mut raw = HashMap::new();
        raw.insert(
            ".".to_string(),
            json!({"env": {"A_KEY": "x", "SHARED_KEY": "x"}}),
        );

        let mut meta = HashMap::new();
        meta.insert(
            "./env.A_KEY".to_string(),
            FieldMeta {
                filename: "apps/current/env.cue".to_string(),
                reference: Some("env.SHARED_KEY".to_string()),
                ..Default::default()
            },
        );
        meta.insert(
            "./env.SHARED_KEY".to_string(),
            FieldMeta {
                filename: "shared/env.cue".to_string(),
                ..Default::default()
            },
        );

        let module =
            ModuleEvaluation::from_raw_with_meta(PathBuf::from("/repo"), raw, vec![], None, meta);

        let env_keys = vec!["A_KEY".to_string()];
        let dirs = resolve_env_definition_dirs(&module, Path::new("."), None, &env_keys);
        assert_eq!(dirs.get("A_KEY"), Some(&PathBuf::from("/repo/shared")));
    }
}
