use super::super::TaskResult;
use super::model::{TaskOutputField, TaskOutputRef};
use crate::{Error, Result};
use std::collections::HashMap;

/// Context for resolving task output reference placeholders at runtime.
pub struct OutputRefResolver<'a> {
    /// Name of the task being resolved (for error messages)
    pub task_name: &'a str,
    /// Completed upstream task results to resolve references against
    pub results: &'a HashMap<String, TaskResult>,
}

impl<'a> OutputRefResolver<'a> {
    /// Resolve all output ref placeholder strings in a task's args and env.
    ///
    /// Called just before task execution. Replaces placeholder strings with
    /// actual values from completed upstream tasks.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - A referenced task has not completed (missing from results)
    /// - A referenced task failed (non-zero exit code)
    /// - An `exitCode` ref is used in a string context (exitCode is int-only)
    pub fn resolve(
        &self,
        args: &mut [String],
        env: &mut HashMap<String, serde_json::Value>,
    ) -> Result<()> {
        // Resolve args
        for arg in args.iter_mut() {
            if let Some(resolved) = resolve_single_ref(self.task_name, arg, self.results)? {
                *arg = resolved;
            }
        }

        // Resolve env values
        for (_env_key, env_val) in env.iter_mut() {
            if let Some(s) = env_val.as_str()
                && let Some(resolved) = resolve_single_ref(self.task_name, s, self.results)?
            {
                *env_val = serde_json::Value::String(resolved);
            }
        }

        Ok(())
    }
}

/// Resolve a single placeholder string, returning the resolved value.
/// Returns Ok(None) if the string is not a placeholder.
fn resolve_single_ref(
    task_name: &str,
    value: &str,
    results: &HashMap<String, TaskResult>,
) -> Result<Option<String>> {
    let Some(output_ref) = TaskOutputRef::parse(value) else {
        return Ok(None);
    };

    // exitCode cannot be used in string context (args/env)
    if output_ref.output == TaskOutputField::ExitCode {
        return Err(Error::configuration(format!(
            "Task '{}': cannot use exitCode of '{}' in args/env (exitCode is an integer, not a string)",
            task_name, output_ref.task
        )));
    }

    let result = results.get(&output_ref.task).ok_or_else(|| {
        Error::configuration(format!(
            "Task '{}': references output of '{}', but that task has not completed",
            task_name, output_ref.task
        ))
    })?;

    if !result.success {
        return Err(Error::task_failed(
            &output_ref.task,
            result.exit_code.unwrap_or(-1),
            &result.stdout,
            &result.stderr,
        ));
    }

    let resolved = match output_ref.output {
        TaskOutputField::Stdout => result.stdout.trim().to_string(),
        TaskOutputField::Stderr => result.stderr.trim().to_string(),
        TaskOutputField::ExitCode => unreachable!("handled above"),
    };

    Ok(Some(resolved))
}
