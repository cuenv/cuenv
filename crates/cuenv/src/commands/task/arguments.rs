//! Task argument parsing and resolution
//!
//! Handles parsing CLI arguments for tasks with parameter definitions,
//! including positional and named arguments, short flags, and interpolation.

use std::collections::HashMap;

use cuenv_core::Result;
use cuenv_core::tasks::{ResolvedArgs, Task, TaskParams};

/// Check if an argument looks like a flag (starts with `-` but is not a negative number)
pub fn looks_like_flag(arg: &str) -> bool {
    if !arg.starts_with('-') {
        return false;
    }
    // Not a flag if it's a negative number (e.g., -1, -3.14)
    let rest = &arg[1..];
    if rest.is_empty() {
        return false;
    }
    // Check if it parses as a number
    rest.parse::<f64>().is_err()
}

/// Parse CLI arguments into positional and named values
/// If params is provided, short flags (-x) are resolved to their long names
/// Supports `--` separator to end flag parsing
pub fn parse_task_args(
    args: &[String],
    params: Option<&TaskParams>,
) -> (Vec<String>, HashMap<String, String>) {
    let mut positional = Vec::new();
    let mut named = HashMap::new();
    let mut flags_ended = false;

    // Build short-to-long flag mapping
    let short_to_long: HashMap<String, String> = params
        .map(|p| {
            p.named
                .iter()
                .filter_map(|(name, def)| def.short.as_ref().map(|s| (s.clone(), name.clone())))
                .collect()
        })
        .unwrap_or_default();

    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];

        // Handle `--` separator: all subsequent args are positional
        if arg == "--" {
            flags_ended = true;
            i += 1;
            continue;
        }

        if flags_ended {
            positional.push(arg.clone());
        } else if arg.starts_with("--") {
            let key = arg.strip_prefix("--").unwrap_or(arg);
            // Check if there's an '=' in the argument (e.g., --key=value)
            if let Some((k, v)) = key.split_once('=') {
                named.insert(k.to_string(), v.to_string());
            } else if i + 1 < args.len() && !looks_like_flag(&args[i + 1]) {
                // Next argument is the value
                named.insert(key.to_string(), args[i + 1].clone());
                i += 1;
            } else {
                // Boolean flag (no value)
                named.insert(key.to_string(), "true".to_string());
            }
        } else if let Some(short_key) = arg.strip_prefix('-') {
            // Short flag handling: must be single char and not a digit
            if short_key.len() == 1 && !short_key.chars().next().unwrap_or('0').is_ascii_digit() {
                let long_key = short_to_long
                    .get(short_key)
                    .cloned()
                    .unwrap_or_else(|| short_key.to_string());

                if i + 1 < args.len() && !looks_like_flag(&args[i + 1]) {
                    named.insert(long_key, args[i + 1].clone());
                    i += 1;
                } else {
                    // Boolean flag
                    named.insert(long_key, "true".to_string());
                }
            } else {
                // Not a valid short flag (multi-char like -abc, or negative number)
                positional.push(arg.clone());
            }
        } else {
            positional.push(arg.clone());
        }
        i += 1;
    }

    (positional, named)
}

/// Validate and resolve arguments against task parameter definitions
pub fn resolve_task_args(
    params: Option<&TaskParams>,
    cli_args: &[String],
) -> Result<ResolvedArgs> {
    let (positional_values, named_values) = parse_task_args(cli_args, params);
    let mut resolved = ResolvedArgs::new();

    if let Some(params) = params {
        // Validate excess positional arguments
        let max_positional = params.positional.len();
        if positional_values.len() > max_positional {
            return Err(cuenv_core::Error::configuration(format!(
                "Too many positional arguments: expected at most {}, got {}",
                max_positional,
                positional_values.len()
            )));
        }

        // Validate unknown named arguments
        let unknown_flags: Vec<String> = named_values
            .keys()
            .filter(|k| !params.named.contains_key(*k))
            .map(|k| format!("--{k}"))
            .collect();
        if !unknown_flags.is_empty() {
            return Err(cuenv_core::Error::configuration(format!(
                "Unknown argument(s): {}",
                unknown_flags.join(", ")
            )));
        }

        // Process positional arguments
        for (i, param_def) in params.positional.iter().enumerate() {
            if let Some(value) = positional_values.get(i) {
                resolved.positional.push(value.clone());
            } else if let Some(default) = &param_def.default {
                resolved.positional.push(default.clone());
            } else if param_def.required {
                let default_desc = format!("positional argument {i}");
                let desc = param_def.description.as_deref().unwrap_or(&default_desc);
                return Err(cuenv_core::Error::configuration(format!(
                    "Missing required argument: {desc}"
                )));
            } else {
                resolved.positional.push(String::new());
            }
        }

        // Process named arguments
        for (name, param_def) in &params.named {
            if let Some(value) = named_values.get(name) {
                resolved.named.insert(name.clone(), value.clone());
            } else if let Some(default) = &param_def.default {
                resolved.named.insert(name.clone(), default.clone());
            } else if param_def.required {
                return Err(cuenv_core::Error::configuration(format!(
                    "Missing required argument: --{name}"
                )));
            }
        }
    } else {
        // No params defined, just pass through all args
        resolved.positional = positional_values;
        resolved.named = named_values;
    }

    Ok(resolved)
}

/// Apply resolved arguments to a task, interpolating placeholders in command and args
pub fn apply_args_to_task(task: &Task, resolved_args: &ResolvedArgs) -> Task {
    let mut new_task = task.clone();

    // Interpolate command
    new_task.command = resolved_args.interpolate(&task.command);

    // Interpolate args
    new_task.args = resolved_args.interpolate_args(&task.args);

    new_task
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_core::tasks::ParamDef;
    use std::collections::HashMap;

    #[test]
    fn test_looks_like_flag() {
        assert!(looks_like_flag("-v"));
        assert!(looks_like_flag("--verbose"));
        assert!(looks_like_flag("-abc"));
        assert!(!looks_like_flag("file.txt"));
        assert!(!looks_like_flag("-1"));
        assert!(!looks_like_flag("-3.14"));
        assert!(!looks_like_flag("-"));
    }

    #[test]
    fn test_parse_task_args_simple() {
        let args: Vec<String> = vec!["pos1".into(), "--flag".into(), "value".into()];
        let (positional, named) = parse_task_args(&args, None);
        assert_eq!(positional, vec!["pos1"]);
        assert_eq!(named.get("flag"), Some(&"value".to_string()));
    }

    #[test]
    fn test_parse_task_args_double_dash() {
        let args: Vec<String> = vec![
            "--flag".into(),
            "value".into(),
            "--".into(),
            "--not-a-flag".into(),
        ];
        let (positional, named) = parse_task_args(&args, None);
        assert_eq!(positional, vec!["--not-a-flag"]);
        assert_eq!(named.get("flag"), Some(&"value".to_string()));
    }

    #[test]
    fn test_parse_task_args_equals_syntax() {
        let args: Vec<String> = vec!["--key=value".into()];
        let (positional, named) = parse_task_args(&args, None);
        assert!(positional.is_empty());
        assert_eq!(named.get("key"), Some(&"value".to_string()));
    }

    #[test]
    fn test_parse_task_args_short_flags() {
        let mut named_params = HashMap::new();
        named_params.insert(
            "verbose".to_string(),
            ParamDef {
                short: Some("v".to_string()),
                ..Default::default()
            },
        );
        let params = TaskParams {
            positional: vec![],
            named: named_params,
        };

        let args: Vec<String> = vec!["-v".into()];
        let (_, named) = parse_task_args(&args, Some(&params));
        assert_eq!(named.get("verbose"), Some(&"true".to_string()));
    }
}
