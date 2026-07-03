use super::{TaskOutputField, TaskOutputRef};
use crate::TaskResult;
use cuenv_core::{Error, Result};
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_result(name: &str, stdout: &str, stderr: &str, exit_code: i32) -> TaskResult {
        TaskResult {
            name: name.to_string(),
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            exit_code: Some(exit_code),
            success: exit_code == 0,
        }
    }

    fn resolver(results: &HashMap<String, TaskResult>) -> OutputRefResolver<'_> {
        OutputRefResolver {
            task_name: "work",
            results,
        }
    }

    #[test]
    fn resolve_stdout_in_args() {
        let mut args = vec!["cuenv:ref:tmpdir:stdout".to_string()];
        let mut env = HashMap::new();
        let mut results = HashMap::new();
        results.insert(
            "tmpdir".to_string(),
            make_result("tmpdir", "/tmp/abc\n", "", 0),
        );

        resolver(&results).resolve(&mut args, &mut env).unwrap();
        assert_eq!(args[0], "/tmp/abc"); // trimmed
    }

    #[test]
    fn resolve_stderr_in_env() {
        let mut args = Vec::new();
        let mut env = HashMap::new();
        env.insert(
            "ERR".to_string(),
            serde_json::Value::String("cuenv:ref:check:stderr".to_string()),
        );
        let mut results = HashMap::new();
        results.insert(
            "check".to_string(),
            make_result("check", "", "  warning  \n", 0),
        );

        resolver(&results).resolve(&mut args, &mut env).unwrap();
        assert_eq!(env["ERR"].as_str().unwrap(), "warning");
    }

    #[test]
    fn resolve_non_ref_strings_unchanged() {
        let mut args = vec!["hello".to_string(), "--flag".to_string()];
        let mut env = HashMap::new();
        env.insert(
            "FOO".to_string(),
            serde_json::Value::String("bar".to_string()),
        );
        let results = HashMap::new();

        resolver(&results).resolve(&mut args, &mut env).unwrap();
        assert_eq!(args, vec!["hello", "--flag"]);
        assert_eq!(env["FOO"].as_str().unwrap(), "bar");
    }

    #[test]
    fn resolve_missing_task_errors() {
        let mut args = vec!["cuenv:ref:nonexistent:stdout".to_string()];
        let mut env = HashMap::new();
        let results = HashMap::new();

        let err = resolver(&results).resolve(&mut args, &mut env).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("nonexistent"));
        assert!(msg.contains("not completed"));
    }

    #[test]
    fn resolve_failed_task_errors() {
        let mut args = vec!["cuenv:ref:failing:stdout".to_string()];
        let mut env = HashMap::new();
        let mut results = HashMap::new();
        results.insert(
            "failing".to_string(),
            make_result("failing", "", "error!", 1),
        );

        let err = resolver(&results).resolve(&mut args, &mut env).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("failing") || msg.contains("failed"));
    }

    #[test]
    fn resolve_exit_code_in_args_errors() {
        let mut args = vec!["cuenv:ref:check:exitCode".to_string()];
        let mut env = HashMap::new();
        let mut results = HashMap::new();
        results.insert("check".to_string(), make_result("check", "", "", 0));

        let err = resolver(&results).resolve(&mut args, &mut env).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("exitCode"));
        assert!(msg.contains("integer"));
    }

    #[test]
    fn resolve_empty_stdout() {
        let mut args = vec!["cuenv:ref:quiet:stdout".to_string()];
        let mut env = HashMap::new();
        let mut results = HashMap::new();
        results.insert("quiet".to_string(), make_result("quiet", "", "", 0));

        resolver(&results).resolve(&mut args, &mut env).unwrap();
        assert_eq!(args[0], ""); // empty after trim
    }

    #[test]
    fn resolve_trimming_behavior() {
        let mut args = vec!["cuenv:ref:padded:stdout".to_string()];
        let mut env = HashMap::new();
        let mut results = HashMap::new();
        results.insert(
            "padded".to_string(),
            make_result("padded", "  hello world  \n\n", "", 0),
        );

        resolver(&results).resolve(&mut args, &mut env).unwrap();
        assert_eq!(args[0], "hello world");
    }
}
