use serde_json::{Map, Value};

use super::ReferenceMap;

pub(super) fn enrich_task_refs(value: &mut Value, instance_path: &str, references: &ReferenceMap) {
    TaskRefContext {
        instance_path,
        references,
    }
    .enrich(value, "");
}

struct TaskRefContext<'a> {
    instance_path: &'a str,
    references: &'a ReferenceMap,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum TaskRefShape {
    ArrayElement,
    Field,
}

impl TaskRefContext<'_> {
    fn enrich(&self, value: &mut Value, field_path: &str) {
        match value {
            Value::Object(obj) => {
                self.enrich_object(obj, field_path);
                self.enrich_children(obj, field_path);
            }
            Value::Array(items) => {
                if is_ci_pipeline_tasks_array(field_path) {
                    self.enrich_task_ref_array(items, field_path);
                }

                for (index, child) in items.iter_mut().enumerate() {
                    self.enrich(child, &format!("{field_path}[{index}]"));
                }
            }
            _ => {}
        }
    }

    fn enrich_object(&self, obj: &mut Map<String, Value>, field_path: &str) {
        if let Some(Value::Array(dependencies)) = obj.get_mut("dependsOn") {
            self.enrich_task_ref_array(dependencies, &child_path(field_path, "dependsOn"));
        }

        if is_ci_matrix_task_object(field_path, obj)
            && let Some(task) = obj.get_mut("task")
        {
            self.enrich_task_ref_value(task, &child_path(field_path, "task"), TaskRefShape::Field);
        }
    }

    fn enrich_children(&self, obj: &mut Map<String, Value>, field_path: &str) {
        for (key, child) in obj.iter_mut() {
            if key == "dependsOn" || key == "task" {
                continue;
            }

            self.enrich(child, &child_path(field_path, key));
        }
    }

    fn enrich_task_ref_array(&self, items: &mut [Value], array_path: &str) {
        for (index, item) in items.iter_mut().enumerate() {
            self.enrich_task_ref_value(
                item,
                &format!("{array_path}[{index}]"),
                TaskRefShape::ArrayElement,
            );
        }
    }

    fn enrich_task_ref_value(&self, value: &mut Value, field_path: &str, shape: TaskRefShape) {
        let Some(name) = self.reference_name(field_path) else {
            return;
        };

        match value {
            Value::Object(obj) => {
                if obj
                    .get("_name")
                    .and_then(Value::as_str)
                    .is_none_or(str::is_empty)
                {
                    obj.insert("_name".to_string(), Value::String(name));
                }
            }
            Value::Array(_) => {
                *value = serde_json::json!({ "_name": name });
            }
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_)
                if shape == TaskRefShape::ArrayElement =>
            {
                *value = serde_json::json!({ "_name": name });
            }
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
        }
    }

    fn reference_name(&self, field_path: &str) -> Option<String> {
        self.references
            .get(&format!("{}/{}", self.instance_path, field_path))
            .map(|reference| strip_dependency_prefix(reference).to_string())
    }
}

fn child_path(parent: &str, child: &str) -> String {
    if parent.is_empty() {
        child.to_string()
    } else {
        format!("{parent}.{child}")
    }
}

fn strip_dependency_prefix(path: &str) -> &str {
    for prefix in [
        "tasks.",
        "_tasks.",
        "_t.",
        "services.",
        "_services.",
        "_s.",
        "images.",
        "_images.",
        "_i.",
    ] {
        if let Some(stripped) = path.strip_prefix(prefix) {
            return stripped;
        }
    }

    if path.starts_with('_') || path.starts_with('#') {
        for infix in [
            ".tasks.",
            "._tasks.",
            "._t.",
            ".services.",
            "._services.",
            "._s.",
            ".images.",
            "._images.",
            "._i.",
        ] {
            if let Some((_, stripped)) = path.split_once(infix) {
                return stripped;
            }
        }
    }

    path
}

fn is_ci_pipeline_tasks_array(field_path: &str) -> bool {
    field_path.starts_with("ci.pipelines.") && field_path.ends_with(".tasks")
}

fn is_ci_matrix_task_object(field_path: &str, obj: &Map<String, Value>) -> bool {
    if !(field_path.starts_with("ci.pipelines.") && field_path.contains(".tasks[")) {
        return false;
    }

    obj.contains_key("matrix") || obj.get("type").and_then(Value::as_str) == Some("matrix")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci::PipelineTask;
    use crate::manifest::Project;
    use crate::module::ModuleEvaluation;
    use crate::tasks::TaskNode;
    use serde_json::json;
    use std::collections::HashMap;
    use std::path::PathBuf;

    #[test]
    fn test_strip_dependency_prefix() {
        assert_eq!(strip_dependency_prefix("tasks.build"), "build");
        assert_eq!(strip_dependency_prefix("tasks.ci.deploy"), "ci.deploy");
        assert_eq!(strip_dependency_prefix("_t.cargo.build"), "cargo.build");
        assert_eq!(strip_dependency_prefix("_tasks.internal"), "internal");
        assert_eq!(strip_dependency_prefix("services.db"), "db");
        assert_eq!(strip_dependency_prefix("_s.db"), "db");
        assert_eq!(strip_dependency_prefix("_services.cache"), "cache");
        assert_eq!(strip_dependency_prefix("images.base"), "base");
        assert_eq!(strip_dependency_prefix("_i.web"), "web");
        assert_eq!(strip_dependency_prefix("build"), "build");
        assert_eq!(strip_dependency_prefix("ci.deploy"), "ci.deploy");
    }

    #[test]
    fn test_strip_dependency_prefix_reusable_definition_path() {
        assert_eq!(strip_dependency_prefix("_svc.tasks.migrate"), "migrate");
        assert_eq!(strip_dependency_prefix("#Svc.tasks.migrate"), "migrate");
        assert_eq!(strip_dependency_prefix("#Svc._tasks.deploy"), "deploy");
        assert_eq!(strip_dependency_prefix("_def.tasks.deploy"), "deploy");
        assert_eq!(
            strip_dependency_prefix("_outer._inner.tasks.group.subtask"),
            "group.subtask"
        );
        assert_eq!(strip_dependency_prefix("_svc.services.db"), "db");
        assert_eq!(strip_dependency_prefix("_svc.images.web"), "web");
        assert_eq!(
            strip_dependency_prefix("pkg.tasks.build"),
            "pkg.tasks.build"
        );
    }

    #[derive(Debug, Clone, Copy)]
    enum TaskNodeShape {
        Task,
        Group,
        Sequence,
    }

    impl TaskNodeShape {
        const fn name(self) -> &'static str {
            match self {
                Self::Task => "task",
                Self::Group => "group",
                Self::Sequence => "sequence",
            }
        }

        fn as_value(self) -> Value {
            match self {
                Self::Task => json!({
                    "command": "echo",
                    "args": ["task"],
                }),
                Self::Group => json!({
                    "type": "group",
                    "step": {
                        "command": "echo",
                        "args": ["group"],
                    },
                }),
                Self::Sequence => json!([
                    {
                        "command": "echo",
                        "args": ["sequence-0"],
                    },
                    {
                        "command": "echo",
                        "args": ["sequence-1"],
                    },
                ]),
            }
        }
    }

    #[derive(Debug, Clone, Copy)]
    enum DependencyOwner {
        Task,
        Group,
    }

    impl DependencyOwner {
        const fn name(self) -> &'static str {
            match self {
                Self::Task => "task",
                Self::Group => "group",
            }
        }

        fn node_with_dependency(self, target: Value) -> Value {
            match self {
                Self::Task => json!({
                    "command": "echo",
                    "args": ["consumer"],
                    "dependsOn": [target],
                }),
                Self::Group => json!({
                    "type": "group",
                    "dependsOn": [target],
                    "step": {
                        "command": "echo",
                        "args": ["consumer"],
                    },
                }),
            }
        }
    }

    fn deserialize_project_with_references(instance: Value, references: ReferenceMap) -> Project {
        let mut raw = HashMap::new();
        raw.insert(".".to_string(), instance);
        let module = ModuleEvaluation::from_raw(
            PathBuf::from("/test"),
            raw,
            vec![".".to_string()],
            Some(references),
        );

        module
            .root_instance()
            .expect("root instance should exist")
            .deserialize::<Project>()
            .expect("project deserialization should succeed")
    }

    #[test]
    fn test_depends_on_accepts_all_task_node_shapes_for_task_and_group() {
        let shapes = [
            TaskNodeShape::Task,
            TaskNodeShape::Group,
            TaskNodeShape::Sequence,
        ];
        let owners = [DependencyOwner::Task, DependencyOwner::Group];

        for owner in owners {
            for shape in shapes {
                let target = shape.as_value();
                let consumer = owner.node_with_dependency(target.clone());

                let instance = json!({
                    "name": "shape-contract",
                    "tasks": {
                        "target": target,
                        "consumer": consumer,
                    },
                });

                let mut references = ReferenceMap::new();
                references.insert(
                    "./tasks.consumer.dependsOn[0]".to_string(),
                    "tasks.target".to_string(),
                );

                let project = deserialize_project_with_references(instance, references);
                let consumer_node = project
                    .tasks
                    .get("consumer")
                    .expect("consumer task should exist");
                let dependency_names: Vec<&str> = consumer_node
                    .depends_on()
                    .iter()
                    .map(|dependency| dependency.task_name())
                    .collect();

                assert_eq!(
                    dependency_names,
                    vec!["target"],
                    "dependsOn owner={} should canonicalize {} reference",
                    owner.name(),
                    shape.name()
                );
            }
        }
    }

    #[test]
    fn test_service_depends_on_canonicalizes_task_and_service_references() {
        let instance = json!({
            "name": "service-contract",
            "tasks": {
                "migrate": {
                    "command": "echo",
                    "args": ["migrate"]
                }
            },
            "services": {
                "db": {
                    "type": "service",
                    "command": "echo",
                    "args": ["db"]
                },
                "seed": {
                    "type": "service",
                    "command": "echo",
                    "args": ["seed"],
                    "dependsOn": ["placeholder-a", "placeholder-b"]
                }
            }
        });

        let mut references = ReferenceMap::new();
        references.insert(
            "./services.seed.dependsOn[0]".to_string(),
            "services.db".to_string(),
        );
        references.insert(
            "./services.seed.dependsOn[1]".to_string(),
            "tasks.migrate".to_string(),
        );

        let project = deserialize_project_with_references(instance, references);
        let seed = project
            .services
            .get("seed")
            .expect("seed service should exist");

        let dep_names: Vec<&str> = seed.depends_on.iter().map(|d| d.task_name()).collect();
        assert_eq!(dep_names, vec!["db", "migrate"]);
    }

    #[test]
    fn test_ci_pipeline_tasks_reference_accepts_all_task_node_shapes() {
        for shape in [
            TaskNodeShape::Task,
            TaskNodeShape::Group,
            TaskNodeShape::Sequence,
        ] {
            let target = shape.as_value();
            let instance = json!({
                "name": "shape-contract",
                "tasks": {
                    "target": target.clone(),
                },
                "ci": {
                    "pipelines": {
                        "default": {
                            "tasks": [target],
                        },
                    },
                },
            });

            let mut references = ReferenceMap::new();
            references.insert(
                "./ci.pipelines.default.tasks[0]".to_string(),
                "tasks.target".to_string(),
            );

            let project = deserialize_project_with_references(instance, references);
            let pipeline = &project
                .ci
                .as_ref()
                .expect("ci should exist")
                .pipelines
                .get("default")
                .expect("default pipeline should exist");
            let pipeline_task = pipeline.tasks.first().expect("pipeline task should exist");

            assert!(
                pipeline_task.is_simple(),
                "pipeline task should remain a simple reference for {}",
                shape.name()
            );
            assert_eq!(
                pipeline_task.task_name(),
                "target",
                "pipeline task reference should canonicalize {} shape",
                shape.name()
            );
        }
    }

    #[test]
    fn test_ci_matrix_task_reference_accepts_all_task_node_shapes() {
        for shape in [
            TaskNodeShape::Task,
            TaskNodeShape::Group,
            TaskNodeShape::Sequence,
        ] {
            let target = shape.as_value();
            let instance = json!({
                "name": "shape-contract",
                "tasks": {
                    "target": target.clone(),
                },
                "ci": {
                    "pipelines": {
                        "default": {
                            "tasks": [
                                {
                                    "type": "matrix",
                                    "task": target,
                                    "matrix": {
                                        "arch": ["linux-x64"],
                                    },
                                },
                            ],
                        },
                    },
                },
            });

            let mut references = ReferenceMap::new();
            references.insert(
                "./ci.pipelines.default.tasks[0].task".to_string(),
                "tasks.target".to_string(),
            );

            let project = deserialize_project_with_references(instance, references);
            let pipeline = &project
                .ci
                .as_ref()
                .expect("ci should exist")
                .pipelines
                .get("default")
                .expect("default pipeline should exist");
            let pipeline_task = pipeline.tasks.first().expect("pipeline task should exist");

            match pipeline_task {
                PipelineTask::Matrix(matrix_task) => {
                    assert_eq!(
                        matrix_task.task.task_name(),
                        "target",
                        "matrix task reference should canonicalize {} shape",
                        shape.name()
                    );
                }
                PipelineTask::Simple(_) | PipelineTask::Node(_) => {
                    panic!("expected matrix task for {}", shape.name())
                }
            }
        }
    }

    #[test]
    fn test_non_ci_task_string_field_is_not_rewritten() {
        let instance = json!({
            "name": "shape-contract",
            "tasks": {
                "producer": {
                    "command": "echo",
                    "args": ["producer"],
                },
                "consumer": {
                    "command": "echo",
                    "args": ["consumer"],
                    "inputs": [
                        {
                            "task": "producer",
                        },
                    ],
                },
            },
        });

        let mut references = ReferenceMap::new();
        references.insert(
            "./tasks.consumer.inputs[0].task".to_string(),
            "tasks.producer".to_string(),
        );

        let project = deserialize_project_with_references(instance, references);
        let consumer_node = project
            .tasks
            .get("consumer")
            .expect("consumer task should exist");
        let consumer_task = match consumer_node {
            TaskNode::Task(task) => task,
            TaskNode::Group(_) | TaskNode::Sequence(_) => {
                panic!("expected consumer to deserialize as a task")
            }
        };

        let task_output = consumer_task
            .iter_task_outputs()
            .next()
            .expect("consumer should have one task output input");
        assert_eq!(
            task_output.task, "producer",
            "non-CI task string fields must remain strings after enrichment"
        );
    }

    #[test]
    fn test_reusable_definition_depends_on_resolved_via_reference_metadata() {
        let migrate_task = json!({"command": "echo", "args": ["migrate"]});
        let deploy_task = json!({
            "command": "echo",
            "args": ["deploy"],
            "dependsOn": [migrate_task.clone()],
        });
        let instance = json!({
            "name": "reusable-def-test",
            "tasks": {
                "migrate": migrate_task,
                "deploy": deploy_task,
            },
        });

        let mut references = ReferenceMap::new();
        references.insert(
            "./tasks.deploy.dependsOn[0]".to_string(),
            "#Svc.tasks.migrate".to_string(),
        );

        let project = deserialize_project_with_references(instance, references);
        let deploy_node = project
            .tasks
            .get("deploy")
            .expect("deploy task should exist");
        let dep_names: Vec<&str> = deploy_node
            .depends_on()
            .iter()
            .map(|d| d.task_name())
            .collect();

        assert_eq!(dep_names, vec!["migrate"]);
    }

    #[test]
    fn test_reusable_definition_ci_pipeline_task_resolved_via_reference_metadata() {
        let migrate_task = json!({"command": "echo", "args": ["migrate"]});
        let deploy_task = json!({
            "command": "echo",
            "args": ["deploy"],
            "dependsOn": [migrate_task.clone()],
        });
        let instance = json!({
            "name": "ci-pipeline-reference-test",
            "tasks": {
                "migrate": migrate_task,
                "deploy": deploy_task.clone(),
            },
            "ci": {
                "pipelines": {
                    "default": {
                        "tasks": [deploy_task],
                    },
                },
            },
        });

        let mut references = ReferenceMap::new();
        references.insert(
            "./tasks.deploy.dependsOn[0]".to_string(),
            "#Svc.tasks.migrate".to_string(),
        );
        references.insert(
            "./ci.pipelines.default.tasks[0]".to_string(),
            "#Svc._tasks.deploy".to_string(),
        );
        references.insert(
            "./ci.pipelines.default.tasks[0].dependsOn[0]".to_string(),
            "#Svc.tasks.migrate".to_string(),
        );

        let project = deserialize_project_with_references(instance, references);
        let pipeline = project
            .ci
            .as_ref()
            .expect("ci should exist")
            .pipelines
            .get("default")
            .expect("default pipeline should exist");
        let pipeline_task = pipeline.tasks.first().expect("pipeline task should exist");
        assert!(pipeline_task.is_simple());
        assert_eq!(pipeline_task.task_name(), "deploy");

        let deploy_node = project
            .tasks
            .get("deploy")
            .expect("deploy task should exist");
        let dep_names: Vec<&str> = deploy_node
            .depends_on()
            .iter()
            .map(|d| d.task_name())
            .collect();
        assert_eq!(dep_names, vec!["migrate"]);
    }
}
