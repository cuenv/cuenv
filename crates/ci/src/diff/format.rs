use super::{ChangeType, DigestDiff};
use std::fmt::Write;

/// Format a diff for human-readable output.
#[must_use]
pub fn format_diff(diff: &DigestDiff) -> String {
    let mut output = String::new();
    let _ = writeln!(
        output,
        "Comparing runs: {} -> {}\n",
        &diff.run_a[..7.min(diff.run_a.len())],
        &diff.run_b[..7.min(diff.run_b.len())]
    );
    output.push_str("Summary:\n");
    let _ = writeln!(output, "  Total tasks: {}", diff.summary.total_tasks);
    let _ = writeln!(output, "  Changed: {}", diff.summary.changed_tasks);
    let _ = writeln!(output, "  Added: {}", diff.summary.added_tasks);
    let _ = writeln!(output, "  Removed: {}", diff.summary.removed_tasks);
    if diff.summary.secret_changes > 0 {
        let _ = writeln!(output, "  Secret changes: {}", diff.summary.secret_changes);
    }
    output.push('\n');

    for task in &diff.task_diffs {
        if task.change_type == ChangeType::Unchanged {
            continue;
        }
        let symbol = match task.change_type {
            ChangeType::Modified => "~",
            ChangeType::CacheInvalidated => "!",
            ChangeType::Added => "+",
            ChangeType::Removed => "-",
            ChangeType::Unchanged => " ",
        };
        let _ = writeln!(output, "{} {}", symbol, task.name);
        if !task.changed_files.is_empty() {
            output.push_str("  Changed files:\n");
            for file in &task.changed_files {
                let _ = writeln!(output, "    - {file}");
            }
        }
        if task.secrets_changed {
            output.push_str("  Secrets: changed (values hidden)\n");
        }
        output.push('\n');
    }
    output
}
