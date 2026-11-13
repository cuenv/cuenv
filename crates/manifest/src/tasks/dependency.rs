use serde::Serialize;

/// A task dependency, by the name the graph knows the task by.
///
/// In CUE a dependency is the task itself (`dependsOn: [build]`, or
/// `[content.tasks.build]` through an import); loading names it `_name`:
/// `build` in the same project, `#content:build` in another. The embedded
/// value is dropped once it is named.
///
/// Supports deserialization from:
/// - A string: `"taskName"` -> `TaskDependency { name: "taskName" }`
/// - An object with `_name`: `{ "_name": "taskName", ... }` -> `TaskDependency { name: "taskName" }`
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TaskDependency {
    /// The task name: `build`, `test.unit` or, for a task of another
    /// project, `#content:build`.
    #[serde(rename = "_name")]
    pub name: String,
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

                // The embedded value has served its purpose once the name is
                // known: its stamp was read when the project was loaded.
                Ok(TaskDependency::from_name(name))
            }
        }

        deserializer.deserialize_any(TaskDependencyVisitor)
    }
}

impl TaskDependency {
    /// Create a new TaskDependency from a task name
    #[must_use]
    pub fn from_name(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }

    /// Get the task name
    #[must_use]
    pub fn task_name(&self) -> &str {
        &self.name
    }

    /// Check if this dependency matches a given task name
    #[must_use]
    pub fn matches(&self, name: &str) -> bool {
        self.name == name
    }
}
