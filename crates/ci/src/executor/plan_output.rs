//! Machine-readable plan of a `cuenv ci --dry-run`.
//!
//! A dry run selects the tasks a run would execute, comparing the run with
//! its base. This module renders that selection for scripts, such as the
//! change detection job of a generated GitHub workflow.

use cuenv_core::Result;
use serde::Serialize;
use std::collections::BTreeSet;
use std::path::PathBuf;

/// How a dry run writes the tasks it selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanFormat {
    /// One JSON object: the selection per project and the names selected overall.
    Json,
    /// The selected task names, sorted, one per line. Nothing selected is empty output.
    Lines,
}

/// Where and how a dry run writes the tasks it selected.
#[derive(Debug, Clone)]
pub struct PlanOutput {
    /// How to render the selection.
    pub format: PlanFormat,
    /// File to write. Without one the plan goes to stdout, next to whatever
    /// else the run prints there.
    pub destination: Option<PathBuf>,
}

/// The tasks one project's pipeline selected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlannedProject {
    /// Project directory.
    pub project: String,
    /// Pipeline that was planned.
    pub pipeline: String,
    /// Selected task names, in pipeline order.
    pub tasks: Vec<String>,
}

#[derive(Serialize)]
struct PlanDocument<'plan> {
    projects: &'plan [PlannedProject],
    tasks: BTreeSet<&'plan str>,
}

/// Render the selection.
///
/// # Errors
///
/// Returns an error if the JSON document cannot be serialised.
pub fn render(format: PlanFormat, projects: &[PlannedProject]) -> Result<String> {
    let names: BTreeSet<&str> = projects
        .iter()
        .flat_map(|project| project.tasks.iter().map(String::as_str))
        .collect();
    match format {
        PlanFormat::Lines => Ok(names.iter().fold(String::new(), |mut lines, name| {
            lines.push_str(name);
            lines.push('\n');
            lines
        })),
        PlanFormat::Json => serde_json::to_string_pretty(&PlanDocument {
            projects,
            tasks: names,
        })
        .map_err(|e| cuenv_core::Error::configuration(format!("Failed to render plan: {e}"))),
    }
}

/// Write the selection to its destination.
///
/// # Errors
///
/// Returns an error if rendering fails or the destination cannot be written.
pub fn write(output: &PlanOutput, projects: &[PlannedProject]) -> Result<()> {
    let rendered = render(output.format, projects)?;
    if let Some(path) = &output.destination {
        std::fs::write(path, rendered)
            .map_err(|e| cuenv_core::Error::io_with_path("write CI plan", path.clone(), e))
    } else {
        cuenv_events::emit_stdout!(rendered.trim_end_matches('\n'));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn planned(tasks: &[&str]) -> PlannedProject {
        PlannedProject {
            project: "apps/web".to_string(),
            pipeline: "default".to_string(),
            tasks: tasks.iter().map(|task| (*task).to_string()).collect(),
        }
    }

    #[test]
    fn lines_are_sorted_unique_and_exact() {
        let rendered = render(
            PlanFormat::Lines,
            &[planned(&["test-integration", "build"]), planned(&["build"])],
        )
        .unwrap();
        assert_eq!(rendered, "build\ntest-integration\n");
    }

    #[test]
    fn nothing_selected_renders_empty_lines_and_empty_json() {
        assert_eq!(render(PlanFormat::Lines, &[]).unwrap(), "");
        let json: serde_json::Value =
            serde_json::from_str(&render(PlanFormat::Json, &[]).unwrap()).unwrap();
        assert_eq!(json["projects"], serde_json::json!([]));
        assert_eq!(json["tasks"], serde_json::json!([]));
    }

    #[test]
    fn json_lists_projects_and_overall_names() {
        let json: serde_json::Value = serde_json::from_str(
            &render(PlanFormat::Json, &[planned(&["test", "build"])]).unwrap(),
        )
        .unwrap();
        assert_eq!(json["projects"][0]["project"], "apps/web");
        assert_eq!(json["projects"][0]["pipeline"], "default");
        assert_eq!(
            json["projects"][0]["tasks"],
            serde_json::json!(["test", "build"])
        );
        assert_eq!(json["tasks"], serde_json::json!(["build", "test"]));
    }

    #[test]
    fn write_puts_the_plan_in_the_named_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("plan.txt");
        write(
            &PlanOutput {
                format: PlanFormat::Lines,
                destination: Some(path.clone()),
            },
            &[planned(&["build"])],
        )
        .unwrap();
        assert_eq!(std::fs::read_to_string(path).unwrap(), "build\n");
    }
}
