/// Prefix for placeholder strings that represent task output references.
/// Format: `cuenv:ref:<task_name>:<output_field>`
pub(crate) const OUTPUT_REF_PREFIX: &str = "cuenv:ref:";

/// Prefix for placeholder strings that represent image output references.
/// Format: `cuenv:image-ref:<image_name>:<ref|digest>`
pub(crate) const IMAGE_REF_PREFIX: &str = "cuenv:image-ref:";

/// Prefix for placeholder strings that represent host env passthrough.
/// Format: `cuenv:passthrough:<var_name>`
pub(crate) const PASSTHROUGH_PREFIX: &str = "cuenv:passthrough:";

/// Which output field of a task is being referenced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskOutputField {
    Stdout,
    Stderr,
    ExitCode,
}

/// A parsed task output reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskOutputRef {
    /// Name of the referenced task (e.g., "tmpdir", "pipeline[0]")
    pub task: String,
    /// Which output field is referenced
    pub output: TaskOutputField,
}

impl TaskOutputRef {
    /// Parse a placeholder string like `"cuenv:ref:tmpdir:stdout"`.
    /// Returns `None` if the string is not a valid output ref placeholder.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        let rest = s.strip_prefix(OUTPUT_REF_PREFIX)?;
        // Find the last ':' to split task name from output field.
        // Task names may contain colons (e.g., FQDNs like "task:proj:build"),
        // dots, and brackets. Output field names (stdout/stderr/exitCode) never
        // contain colons, so rfind(':') reliably finds the boundary.
        let last_colon = rest.rfind(':')?;
        let task = &rest[..last_colon];
        let output_str = &rest[last_colon + 1..];

        if task.is_empty() {
            return None;
        }

        let output = match output_str {
            "stdout" => TaskOutputField::Stdout,
            "stderr" => TaskOutputField::Stderr,
            "exitCode" => TaskOutputField::ExitCode,
            _ => return None,
        };

        Some(Self {
            task: task.to_string(),
            output,
        })
    }

    /// Convert to a placeholder string.
    #[must_use]
    pub fn to_placeholder(&self) -> String {
        let output_str = match self.output {
            TaskOutputField::Stdout => "stdout",
            TaskOutputField::Stderr => "stderr",
            TaskOutputField::ExitCode => "exitCode",
        };
        format!("{OUTPUT_REF_PREFIX}{}:{output_str}", self.task)
    }
}

/// A dependency pair: (task_that_references, task_being_referenced).
pub type OutputRefDep = (String, String);
