use serde::Serialize;

/// What kind of node a [`TaskDependency`] refers to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DependencyKind {
    /// A task or task group (the default for string and unstamped dependencies).
    #[default]
    Task,
    /// A `#Service` dependency (bound via Dagger `withServiceBinding` or ephemeral host service).
    Service,
    /// A `#ContainerImage` dependency.
    Image,
}

impl DependencyKind {
    fn from_value(value: &serde_json::Value) -> Self {
        if let Some(kind) = value
            .get("cuenvRef")
            .and_then(|r| r.get("kind"))
            .and_then(serde_json::Value::as_str)
        {
            return match kind {
                "service" => Self::Service,
                "image" => Self::Image,
                _ => Self::Task,
            };
        }
        if value.get("kind").and_then(serde_json::Value::as_str) == Some("service")
            || value.get("type").and_then(serde_json::Value::as_str) == Some("service")
        {
            return Self::Service;
        }
        if value.get("kind").and_then(serde_json::Value::as_str) == Some("image") {
            return Self::Image;
        }
        Self::Task
    }
}

/// A task dependency, by the name the graph knows the node by.
///
/// In CUE a dependency is the node itself (`dependsOn: [build]`,
/// `[services.postgres]`, or `[content.tasks.build]` through an import);
/// loading names it `_name`: `build` in the same project, `#content:build` in
/// another. The embedded value is dropped once it is named, while `kind`
/// remembers whether it is a task, service, or image.
///
/// Supports deserialization from:
/// - A string: `"taskName"` -> `TaskDependency { name: "taskName", kind: Task }`
/// - An object with `_name`: `{ "_name": "taskName", "cuenvRef": {"kind": "..."}, ... }`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskDependency {
    /// The dependency name: `build`, `test.unit`, `postgres`, or `#content:build`.
    pub name: String,
    /// What kind of node this dependency refers to.
    pub kind: DependencyKind,
}

impl Serialize for TaskDependency {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct;
        let field_count = if self.kind == DependencyKind::Task {
            1
        } else {
            2
        };
        let mut state = serializer.serialize_struct("TaskDependency", field_count)?;
        state.serialize_field("_name", &self.name)?;
        if self.kind != DependencyKind::Task {
            state.serialize_field("kind", &self.kind)?;
        }
        state.end()
    }
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
                // Deserialize as a JSON object and extract _name + kind
                let value: serde_json::Value =
                    serde::Deserialize::deserialize(de::value::MapAccessDeserializer::new(map))?;

                let name = value
                    .get("_name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| de::Error::missing_field("_name"))?
                    .to_string();

                let kind = DependencyKind::from_value(&value);

                // The embedded value has served its purpose once the name and
                // kind are known: its stamp was read when the project was loaded.
                Ok(TaskDependency { name, kind })
            }
        }

        deserializer.deserialize_any(TaskDependencyVisitor)
    }
}

impl TaskDependency {
    /// Create a new task [`TaskDependency`] from a task name.
    #[must_use]
    pub fn from_name(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            kind: DependencyKind::Task,
        }
    }

    /// Create a new service [`TaskDependency`] from a service name.
    #[must_use]
    pub fn from_service_name(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            kind: DependencyKind::Service,
        }
    }

    /// Get the dependency name.
    #[must_use]
    pub fn task_name(&self) -> &str {
        &self.name
    }

    /// Get the dependency kind.
    #[must_use]
    pub const fn kind(&self) -> DependencyKind {
        self.kind
    }

    /// Whether this dependency is a task or group.
    #[must_use]
    pub const fn is_task(&self) -> bool {
        matches!(self.kind, DependencyKind::Task)
    }

    /// Whether this dependency is a service.
    #[must_use]
    pub const fn is_service(&self) -> bool {
        matches!(self.kind, DependencyKind::Service)
    }

    /// Check if this dependency matches a given name.
    #[must_use]
    pub fn matches(&self, name: &str) -> bool {
        self.name == name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deserializes_service_dependency_from_cuenv_ref() {
        let json = serde_json::json!({
            "_name": "postgres",
            "type": "service",
            "cuenvRef": {"kind": "service", "project": "app", "name": "postgres"}
        });
        let dep: TaskDependency = serde_json::from_value(json).expect("valid dependency");
        assert_eq!(dep.name, "postgres");
        assert_eq!(dep.kind, DependencyKind::Service);
        assert!(dep.is_service());
        assert!(!dep.is_task());
    }

    #[test]
    fn deserializes_plain_string_as_task_dependency() {
        let dep: TaskDependency = serde_json::from_str("\"build\"").expect("valid string dep");
        assert_eq!(dep.name, "build");
        assert_eq!(dep.kind, DependencyKind::Task);
        assert!(dep.is_task());
    }
}
