use super::graph_walk::WalkOutcome;

/// Task execution result
#[derive(Debug, Clone)]
pub struct TaskResult {
    pub name: String,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
}

impl WalkOutcome for TaskResult {
    fn is_success(&self) -> bool {
        self.success
    }
}

/// Number of lines from stdout/stderr to include when summarizing failures
pub const TASK_FAILURE_SNIPPET_LINES: usize = 20;

/// Build a compact, user-friendly summary for a failed task, including the
/// exit code and the tail of stdout/stderr to help with diagnostics.
pub fn summarize_task_failure(result: &TaskResult, max_output_lines: usize) -> String {
    let exit_code = result
        .exit_code
        .map(|c| c.to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let mut sections = Vec::new();
    sections.push(format!(
        "Task '{}' failed with exit code {}.",
        result.name, exit_code
    ));

    let output = format_failure_streams(result, max_output_lines);
    if output.is_empty() {
        sections.push(
            "No stdout/stderr were captured; rerun with RUST_LOG=debug to stream task logs."
                .to_string(),
        );
    } else {
        sections.push(output);
    }

    sections.join("\n\n")
}

pub(super) fn format_failure_streams(result: &TaskResult, max_output_lines: usize) -> String {
    let mut streams = Vec::new();

    if let Some(stdout) = summarize_stream("stdout", &result.stdout, max_output_lines) {
        streams.push(stdout);
    }

    if let Some(stderr) = summarize_stream("stderr", &result.stderr, max_output_lines) {
        streams.push(stderr);
    }

    streams.join("\n\n")
}

pub(super) fn summarize_stream(
    label: &str,
    content: &str,
    max_output_lines: usize,
) -> Option<String> {
    let normalized = content.trim_end();
    if normalized.is_empty() {
        return None;
    }

    let lines: Vec<&str> = normalized.lines().collect();
    let total = lines.len();
    let start = total.saturating_sub(max_output_lines);
    let snippet = lines[start..].join("\n");

    let header = if total > max_output_lines {
        format!("{label} (last {max_output_lines} of {total} lines):")
    } else {
        format!("{label}:")
    };

    Some(format!("{header}\n{snippet}"))
}
