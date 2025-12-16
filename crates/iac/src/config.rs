//! Configuration types for IaC resource definitions.
//!
//! This module handles CUE configuration loading and dependency graph extraction.

use std::collections::HashMap;
use std::path::Path;

use petgraph::graph::{DiGraph, NodeIndex};
use serde::{Deserialize, Serialize};
use tracing::instrument;

use crate::error::{Error, Result};

/// IaC configuration loaded from CUE.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IacConfig {
    /// Provider configurations
    #[serde(default)]
    pub providers: HashMap<String, ProviderDefinition>,

    /// Resource definitions
    #[serde(default)]
    pub resources: Vec<ResourceDefinition>,

    /// Data source definitions
    #[serde(default)]
    pub data_sources: Vec<DataSourceDefinition>,

    /// Variable definitions
    #[serde(default)]
    pub variables: HashMap<String, VariableDefinition>,

    /// Output definitions
    #[serde(default)]
    pub outputs: HashMap<String, OutputDefinition>,
}

impl IacConfig {
    /// Loads configuration from a CUE file.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the CUE configuration file
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be read or parsed.
    #[instrument(name = "iac_config_load")]
    pub async fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();

        if !path.exists() {
            return Err(Error::ConfigNotFound {
                path: path.to_path_buf(),
            });
        }

        // Use cuengine to evaluate the CUE configuration
        // For now, we'll use the CLI approach as recommended in the analysis
        let output = tokio::process::Command::new("cue")
            .args(["export", "--out", "json"])
            .arg(path)
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(Error::CueParse {
                message: stderr.to_string(),
                path: Some(path.to_path_buf()),
            });
        }

        let config: Self = serde_json::from_slice(&output.stdout)?;
        config.validate()?;

        Ok(config)
    }

    /// Loads configuration from a JSON string (for testing).
    ///
    /// # Errors
    ///
    /// Returns an error if the JSON cannot be parsed.
    pub fn from_json(json: &str) -> Result<Self> {
        let config: Self = serde_json::from_str(json)?;
        config.validate()?;
        Ok(config)
    }

    /// Validates the configuration.
    fn validate(&self) -> Result<()> {
        // Check for duplicate resource IDs
        let mut seen_ids = std::collections::HashSet::new();
        for resource in &self.resources {
            if !seen_ids.insert(&resource.id) {
                return Err(Error::InvalidConfig {
                    message: format!("Duplicate resource ID: {}", resource.id),
                });
            }
        }

        // Validate provider references
        for resource in &self.resources {
            let provider_name = resource.provider.split('_').next().unwrap_or(&resource.provider);
            if !self.providers.contains_key(provider_name) && !self.providers.is_empty() {
                // Only warn if providers are explicitly configured
                tracing::warn!(
                    provider = provider_name,
                    resource = resource.id,
                    "Resource references unconfigured provider"
                );
            }
        }

        Ok(())
    }

    /// Builds a dependency graph from the resource definitions.
    ///
    /// # Errors
    ///
    /// Returns an error if dependencies reference non-existent resources.
    pub fn build_dependency_graph(&self) -> Result<DiGraph<String, ()>> {
        let mut graph = DiGraph::new();
        let mut node_map: HashMap<String, NodeIndex> = HashMap::new();

        // Add all resources as nodes
        for resource in &self.resources {
            let idx = graph.add_node(resource.id.clone());
            node_map.insert(resource.id.clone(), idx);
        }

        // Add dependency edges
        for resource in &self.resources {
            let target_idx = node_map[&resource.id];

            for dep in &resource.depends_on {
                let source_idx = node_map.get(&dep.resource_id).ok_or_else(|| {
                    Error::DependencyResolutionFailed {
                        resource_id: resource.id.clone(),
                        dependency: dep.resource_id.clone(),
                    }
                })?;

                graph.add_edge(*source_idx, target_idx, ());
            }
        }

        Ok(graph)
    }

    /// Extracts resource dependencies from CUE references.
    ///
    /// This walks the configuration looking for references like `resource.id`
    /// patterns that indicate dependencies.
    pub fn extract_implicit_dependencies(&mut self) -> Result<()> {
        let resource_ids: std::collections::HashSet<_> =
            self.resources.iter().map(|r| r.id.clone()).collect();

        for resource in &mut self.resources {
            let implicit_deps = find_references_in_value(&resource.config, &resource_ids);

            for dep_id in implicit_deps {
                if dep_id != resource.id {
                    let dep_ref = ResourceRef {
                        resource_id: dep_id,
                        attribute: None,
                    };

                    if !resource.depends_on.contains(&dep_ref) {
                        resource.depends_on.push(dep_ref);
                    }
                }
            }
        }

        Ok(())
    }
}

/// Provider configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderDefinition {
    /// Provider source (e.g., "hashicorp/aws")
    pub source: String,

    /// Provider version constraint
    #[serde(default)]
    pub version: Option<String>,

    /// Provider configuration
    #[serde(default)]
    pub config: serde_json::Value,

    /// Alias for multiple provider configurations
    #[serde(default)]
    pub alias: Option<String>,
}

/// Resource definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceDefinition {
    /// Unique resource identifier
    pub id: String,

    /// Resource type (e.g., "aws_instance")
    pub type_name: String,

    /// Provider name
    #[serde(default)]
    pub provider: String,

    /// Resource configuration
    #[serde(default)]
    pub config: serde_json::Value,

    /// Explicit dependencies
    #[serde(default)]
    pub depends_on: Vec<ResourceRef>,

    /// Lifecycle configuration
    #[serde(default)]
    pub lifecycle: LifecycleConfig,

    /// Provisioners to run
    #[serde(default)]
    pub provisioners: Vec<ProvisionerConfig>,

    /// Count for creating multiple instances
    #[serde(default)]
    pub count: Option<usize>,

    /// For-each for creating instances from a map
    #[serde(default)]
    pub for_each: Option<serde_json::Value>,
}

impl Default for ResourceDefinition {
    fn default() -> Self {
        Self {
            id: String::new(),
            type_name: String::new(),
            provider: String::new(),
            config: serde_json::Value::Null,
            depends_on: Vec::new(),
            lifecycle: LifecycleConfig::default(),
            provisioners: Vec::new(),
            count: None,
            for_each: None,
        }
    }
}

/// Reference to another resource.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ResourceRef {
    /// Referenced resource ID
    pub resource_id: String,

    /// Optional attribute path
    #[serde(default)]
    pub attribute: Option<String>,
}

/// Data source definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataSourceDefinition {
    /// Unique data source identifier
    pub id: String,

    /// Data source type (e.g., "aws_ami")
    pub type_name: String,

    /// Provider name
    #[serde(default)]
    pub provider: String,

    /// Data source configuration (filter criteria)
    #[serde(default)]
    pub config: serde_json::Value,

    /// Explicit dependencies
    #[serde(default)]
    pub depends_on: Vec<ResourceRef>,
}

/// Variable definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariableDefinition {
    /// Variable type
    #[serde(default)]
    pub var_type: Option<String>,

    /// Default value
    #[serde(default)]
    pub default: Option<serde_json::Value>,

    /// Description
    #[serde(default)]
    pub description: Option<String>,

    /// Validation rules
    #[serde(default)]
    pub validation: Vec<ValidationRule>,

    /// Whether the variable is sensitive
    #[serde(default)]
    pub sensitive: bool,

    /// Whether the variable is nullable
    #[serde(default = "default_true")]
    pub nullable: bool,
}

fn default_true() -> bool {
    true
}

/// Output definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputDefinition {
    /// Output value expression
    pub value: serde_json::Value,

    /// Description
    #[serde(default)]
    pub description: Option<String>,

    /// Whether the output is sensitive
    #[serde(default)]
    pub sensitive: bool,

    /// Explicit dependencies
    #[serde(default)]
    pub depends_on: Vec<ResourceRef>,
}

/// Lifecycle configuration for a resource.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LifecycleConfig {
    /// Create replacement before destroying old resource
    #[serde(default)]
    pub create_before_destroy: bool,

    /// Prevent destruction of the resource
    #[serde(default)]
    pub prevent_destroy: bool,

    /// Ignore changes to specified attributes
    #[serde(default)]
    pub ignore_changes: Vec<String>,

    /// Replace when any of these values change
    #[serde(default)]
    pub replace_triggered_by: Vec<String>,
}

/// Provisioner configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvisionerConfig {
    /// Provisioner type (e.g., "local-exec", "remote-exec")
    pub provisioner_type: String,

    /// Provisioner configuration
    #[serde(default)]
    pub config: serde_json::Value,

    /// When to run (create or destroy)
    #[serde(default)]
    pub when: ProvisionerWhen,

    /// Failure behavior
    #[serde(default)]
    pub on_failure: ProvisionerOnFailure,
}

/// When to run a provisioner.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProvisionerWhen {
    /// Run on create (default)
    #[default]
    Create,
    /// Run on destroy
    Destroy,
}

/// How to handle provisioner failure.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProvisionerOnFailure {
    /// Fail the operation (default)
    #[default]
    Fail,
    /// Continue despite failure
    Continue,
}

/// Validation rule for variables.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationRule {
    /// Condition expression
    pub condition: String,

    /// Error message if validation fails
    pub error_message: String,
}

/// Finds resource references in a JSON value.
///
/// This scans string values for patterns like `${resource.attribute}`
/// or CUE-style `resource.attribute` references.
fn find_references_in_value(
    value: &serde_json::Value,
    resource_ids: &std::collections::HashSet<String>,
) -> Vec<String> {
    let mut refs = Vec::new();

    match value {
        serde_json::Value::String(s) => {
            // Look for ${resource.attr} patterns (Terraform-style)
            let re = regex::Regex::new(r"\$\{(\w+)\.").ok();
            if let Some(re) = re {
                for cap in re.captures_iter(s) {
                    if let Some(m) = cap.get(1) {
                        let id = m.as_str().to_string();
                        if resource_ids.contains(&id) {
                            refs.push(id);
                        }
                    }
                }
            }

            // Look for direct resource.attr patterns (CUE-style)
            for resource_id in resource_ids {
                if s.contains(&format!("{resource_id}.")) {
                    refs.push(resource_id.clone());
                }
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                refs.extend(find_references_in_value(item, resource_ids));
            }
        }
        serde_json::Value::Object(obj) => {
            for v in obj.values() {
                refs.extend(find_references_in_value(v, resource_ids));
            }
        }
        _ => {}
    }

    refs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_dependency_graph() {
        let config = IacConfig {
            providers: HashMap::new(),
            resources: vec![
                ResourceDefinition {
                    id: "vpc".to_string(),
                    type_name: "aws_vpc".to_string(),
                    ..Default::default()
                },
                ResourceDefinition {
                    id: "subnet".to_string(),
                    type_name: "aws_subnet".to_string(),
                    depends_on: vec![ResourceRef {
                        resource_id: "vpc".to_string(),
                        attribute: Some("id".to_string()),
                    }],
                    ..Default::default()
                },
            ],
            data_sources: Vec::new(),
            variables: HashMap::new(),
            outputs: HashMap::new(),
        };

        let graph = config.build_dependency_graph().unwrap();
        assert_eq!(graph.node_count(), 2);
        assert_eq!(graph.edge_count(), 1);
    }

    #[test]
    fn test_find_references() {
        let value = serde_json::json!({
            "subnet_id": "${vpc.id}",
            "tags": {
                "Name": "my-resource"
            }
        });

        let resource_ids: std::collections::HashSet<_> =
            ["vpc".to_string(), "subnet".to_string()].into_iter().collect();

        let refs = find_references_in_value(&value, &resource_ids);
        assert_eq!(refs, vec!["vpc".to_string()]);
    }
}
