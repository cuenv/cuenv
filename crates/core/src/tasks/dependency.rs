use serde::Serialize;

/// A task dependency - an embedded task reference with _name field
/// When tasks reference other tasks directly in CUE (e.g., `dependsOn: [build]`),
/// the Go bridge injects the `_name` field to identify the dependency.
///
/// Supports deserialization from:
/// - A string: `"taskName"` -> `TaskDependency { name: "taskName" }`
/// - An object with `_name`: `{ "_name": "taskName", ... }` -> `TaskDependency { name: "taskName" }`
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TaskDependency {
    /// The task name (injected by Go bridge based on task path)
    /// e.g., "build", "test.unit", "deploy.staging"
    #[serde(rename = "_name")]
    pub name: String,

    // Other fields are captured but not used - we only need the name
    #[serde(flatten)]
    _rest: serde_json::Value,
}

impl<'de> serde::Deserialize<'de> for TaskDependency {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::{self, Visitor};

        struct TaskDependencyVisitor;

        impl<'de> Visitor<'de> for TaskDependencyVisitor {
            type Value = TaskDependency;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a string or an object with _name field")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(TaskDependency::from_name(value))
            }

            fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(TaskDependency::from_name(value))
            }

            fn visit_map<M>(self, map: M) -> Result<Self::Value, M::Error>
            where
                M: de::MapAccess<'de>,
            {
                // Deserialize as a JSON object and extract _name
                let value: serde_json::Value =
                    serde::Deserialize::deserialize(de::value::MapAccessDeserializer::new(map))?;

                let name = value
                    .get("_name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| de::Error::missing_field("_name"))?
                    .to_string();

                Ok(TaskDependency { name, _rest: value })
            }
        }

        deserializer.deserialize_any(TaskDependencyVisitor)
    }
}

impl TaskDependency {
    /// Create a new TaskDependency from a task name
    #[must_use]
    pub fn from_name(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            _rest: serde_json::Value::Null,
        }
    }

    /// Get the task name
    #[must_use]
    pub fn task_name(&self) -> &str {
        &self.name
    }

    /// Check if this dependency matches a given task name
    pub fn matches(&self, name: &str) -> bool {
        self.name == name
    }
}
