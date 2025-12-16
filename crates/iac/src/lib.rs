//! # cuenv-iac
//!
//! CUE-powered Infrastructure as Code actor system with Terraform provider integration.
//!
//! This crate provides a type-safe, actor-based infrastructure management system that:
//! - Uses CUE for configuration and dependency graph extraction
//! - Integrates with Terraform providers via gRPC (tfplugin6 protocol)
//! - Orchestrates resource lifecycle using the Ractor actor framework
//! - Detects infrastructure drift via cloud audit logs and polling
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │  CUE Configuration Layer                                    │
//! │  (Resource definitions → Dependency DAG extraction)         │
//! ├─────────────────────────────────────────────────────────────┤
//! │  Actor Orchestration Layer (Ractor)                         │
//! │  ResourceActor, DependencyManager, SupervisorActor          │
//! ├─────────────────────────────────────────────────────────────┤
//! │  Terraform Provider Layer                                   │
//! │  (gRPC client, go-plugin handshake, DynamicValue encoding)  │
//! ├─────────────────────────────────────────────────────────────┤
//! │  Drift Detection Layer                                      │
//! │  (Audit log streaming + polling reconciliation)             │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Example
//!
//! ```ignore
//! use cuenv_iac::{IacSystem, ResourceConfig};
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     // Initialize the IaC system
//!     let system = IacSystem::new().await?;
//!
//!     // Load configuration from CUE
//!     system.load_config("infrastructure.cue").await?;
//!
//!     // Plan changes
//!     let plan = system.plan().await?;
//!
//!     // Apply changes
//!     system.apply(plan).await?;
//!
//!     Ok(())
//! }
//! ```

#![warn(missing_docs)]
#![warn(clippy::all)]
#![warn(clippy::pedantic)]

pub mod actor;
pub mod config;
pub mod drift;
pub mod error;
pub mod events;
pub mod provider;

// Re-export tfplugin6 generated code
pub mod proto;

pub use actor::{
    DependencyManagerActor, IacSupervisor, ResourceActor, ResourceActorMessage, ResourceState,
};
pub use config::{IacConfig, ResourceDefinition, ResourceRef};
pub use drift::{DriftDetector, DriftEvent, DriftStatus};
pub use error::{Error, Result};
pub use provider::{ProviderClient, ProviderConfig, ProviderManager};

use std::path::Path;
use std::sync::Arc;

use dashmap::DashMap;
use petgraph::graph::DiGraph;
use ractor::ActorRef;
use tokio::sync::RwLock;
use tracing::instrument;

/// The main IaC system coordinator.
///
/// This is the primary entry point for infrastructure management operations.
/// It coordinates CUE configuration loading, actor lifecycle, and Terraform
/// provider interactions.
pub struct IacSystem {
    /// Actor system supervisor
    supervisor: ActorRef<actor::SupervisorMessage>,

    /// Provider manager for Terraform provider lifecycle
    provider_manager: Arc<ProviderManager>,

    /// Resource registry mapping resource IDs to their actors
    resources: Arc<DashMap<String, ActorRef<ResourceActorMessage>>>,

    /// Dependency graph of resources
    dependency_graph: Arc<RwLock<DiGraph<String, ()>>>,

    /// Drift detector for monitoring infrastructure state
    drift_detector: Option<Arc<DriftDetector>>,

    /// Configuration loaded from CUE
    config: Arc<RwLock<Option<IacConfig>>>,
}

impl IacSystem {
    /// Creates a new IaC system with default configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if the actor system fails to initialize.
    #[instrument(name = "iac_system_new")]
    pub async fn new() -> Result<Self> {
        Self::with_config(IacSystemConfig::default()).await
    }

    /// Creates a new IaC system with custom configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if the actor system fails to initialize.
    #[instrument(name = "iac_system_new_with_config", skip(config))]
    pub async fn with_config(config: IacSystemConfig) -> Result<Self> {
        // Start the supervisor actor
        let (supervisor, _handle) = actor::start_supervisor(config.max_concurrent_resources).await?;

        // Initialize the provider manager
        let provider_manager = Arc::new(ProviderManager::new(config.provider_cache_dir.clone()));

        // Initialize drift detector if enabled
        let drift_detector = if config.enable_drift_detection {
            Some(Arc::new(DriftDetector::new(config.drift_config.clone())))
        } else {
            None
        };

        Ok(Self {
            supervisor,
            provider_manager,
            resources: Arc::new(DashMap::new()),
            dependency_graph: Arc::new(RwLock::new(DiGraph::new())),
            drift_detector,
            config: Arc::new(RwLock::new(None)),
        })
    }

    /// Loads infrastructure configuration from a CUE file.
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the CUE configuration file
    ///
    /// # Errors
    ///
    /// Returns an error if the CUE file cannot be read or parsed.
    #[instrument(name = "iac_load_config", skip(self))]
    pub async fn load_config(&self, path: impl AsRef<Path>) -> Result<()> {
        let config = IacConfig::load(path).await?;

        // Update the dependency graph
        let mut graph = self.dependency_graph.write().await;
        *graph = config.build_dependency_graph()?;

        // Store the configuration
        let mut stored_config = self.config.write().await;
        *stored_config = Some(config);

        Ok(())
    }

    /// Plans changes to infrastructure based on the loaded configuration.
    ///
    /// This compares the desired state (from CUE configuration) against the
    /// current state (from Terraform providers) and generates a plan of changes.
    ///
    /// # Errors
    ///
    /// Returns an error if no configuration is loaded or planning fails.
    #[instrument(name = "iac_plan", skip(self))]
    pub async fn plan(&self) -> Result<Plan> {
        let config = self.config.read().await;
        let config = config.as_ref().ok_or(Error::NoConfigLoaded)?;

        let mut plan = Plan::new();

        // Plan each resource
        for resource in &config.resources {
            let resource_plan = self.plan_resource(resource).await?;
            plan.add_resource_plan(resource_plan);
        }

        Ok(plan)
    }

    /// Applies a plan to create, update, or destroy infrastructure resources.
    ///
    /// Resources are applied in dependency order using the actor system for
    /// concurrent execution where possible.
    ///
    /// # Errors
    ///
    /// Returns an error if any resource operation fails.
    #[instrument(name = "iac_apply", skip(self, plan))]
    pub async fn apply(&self, plan: Plan) -> Result<ApplyResult> {
        let graph = self.dependency_graph.read().await;
        let execution_order = self.topological_sort(&graph)?;

        let mut result = ApplyResult::new();

        for resource_id in execution_order {
            if let Some(resource_plan) = plan.resources.get(&resource_id) {
                let resource_result = self.apply_resource(resource_plan).await?;
                result.add_resource_result(resource_id, resource_result);
            }
        }

        Ok(result)
    }

    /// Refreshes the state of all managed resources from the cloud providers.
    ///
    /// # Errors
    ///
    /// Returns an error if any resource refresh fails.
    #[instrument(name = "iac_refresh", skip(self))]
    pub async fn refresh(&self) -> Result<()> {
        for entry in self.resources.iter() {
            let actor = entry.value();
            actor.cast(ResourceActorMessage::Refresh)?;
        }
        Ok(())
    }

    /// Destroys all managed infrastructure resources in reverse dependency order.
    ///
    /// # Errors
    ///
    /// Returns an error if any resource destruction fails.
    #[instrument(name = "iac_destroy", skip(self))]
    pub async fn destroy(&self) -> Result<()> {
        let graph = self.dependency_graph.read().await;
        let execution_order = self.topological_sort(&graph)?;

        // Destroy in reverse order
        for resource_id in execution_order.into_iter().rev() {
            if let Some(actor) = self.resources.get(&resource_id) {
                actor.cast(ResourceActorMessage::Destroy)?;
            }
        }

        Ok(())
    }

    /// Returns the current drift status for all resources.
    ///
    /// # Errors
    ///
    /// Returns an error if drift detection is not enabled.
    #[instrument(name = "iac_drift_status", skip(self))]
    pub async fn drift_status(&self) -> Result<Vec<DriftStatus>> {
        let detector = self
            .drift_detector
            .as_ref()
            .ok_or(Error::DriftDetectionDisabled)?;

        detector.check_all().await
    }

    // Private helper methods

    async fn plan_resource(&self, resource: &ResourceDefinition) -> Result<ResourcePlan> {
        let provider = self
            .provider_manager
            .get_or_start(&resource.provider)
            .await?;

        let current_state = provider.read_resource(&resource.type_name, &resource.id).await?;

        let action = if current_state.is_none() {
            ResourceAction::Create
        } else if resource.config != current_state.unwrap() {
            ResourceAction::Update
        } else {
            ResourceAction::NoOp
        };

        Ok(ResourcePlan {
            resource_id: resource.id.clone(),
            resource_type: resource.type_name.clone(),
            action,
            config: resource.config.clone(),
        })
    }

    async fn apply_resource(&self, plan: &ResourcePlan) -> Result<ResourceResult> {
        // Get or create the resource actor
        let actor = self.get_or_create_resource_actor(plan).await?;

        // Send the appropriate message based on the action
        match plan.action {
            ResourceAction::Create => {
                actor.cast(ResourceActorMessage::Create(plan.config.clone()))?;
            }
            ResourceAction::Update => {
                actor.cast(ResourceActorMessage::Update(plan.config.clone()))?;
            }
            ResourceAction::Destroy => {
                actor.cast(ResourceActorMessage::Destroy)?;
            }
            ResourceAction::NoOp => {}
        }

        Ok(ResourceResult {
            resource_id: plan.resource_id.clone(),
            success: true,
            message: None,
        })
    }

    async fn get_or_create_resource_actor(
        &self,
        plan: &ResourcePlan,
    ) -> Result<ActorRef<ResourceActorMessage>> {
        if let Some(actor) = self.resources.get(&plan.resource_id) {
            return Ok(actor.clone());
        }

        // Create a new resource actor
        let provider = self
            .provider_manager
            .get_or_start(&plan.resource_type)
            .await?;

        let (actor, _handle) = actor::start_resource_actor(
            plan.resource_id.clone(),
            plan.resource_type.clone(),
            provider,
            self.supervisor.clone(),
        )
        .await?;

        self.resources.insert(plan.resource_id.clone(), actor.clone());

        Ok(actor)
    }

    fn topological_sort(
        &self,
        graph: &DiGraph<String, ()>,
    ) -> Result<Vec<String>> {
        use petgraph::algo::toposort;

        let sorted = toposort(graph, None).map_err(|_| Error::CyclicDependency)?;

        Ok(sorted
            .into_iter()
            .map(|idx| graph[idx].clone())
            .collect())
    }
}

/// Configuration for the IaC system.
#[derive(Debug, Clone)]
pub struct IacSystemConfig {
    /// Maximum number of resources to manage concurrently
    pub max_concurrent_resources: usize,

    /// Directory for caching provider binaries
    pub provider_cache_dir: Option<std::path::PathBuf>,

    /// Enable drift detection
    pub enable_drift_detection: bool,

    /// Drift detection configuration
    pub drift_config: drift::DriftConfig,
}

impl Default for IacSystemConfig {
    fn default() -> Self {
        Self {
            max_concurrent_resources: 10,
            provider_cache_dir: None,
            enable_drift_detection: false,
            drift_config: drift::DriftConfig::default(),
        }
    }
}

/// A plan representing pending infrastructure changes.
#[derive(Debug, Clone)]
pub struct Plan {
    /// Resource plans keyed by resource ID
    pub resources: std::collections::HashMap<String, ResourcePlan>,
}

impl Plan {
    /// Creates a new empty plan.
    #[must_use]
    pub fn new() -> Self {
        Self {
            resources: std::collections::HashMap::new(),
        }
    }

    /// Adds a resource plan.
    pub fn add_resource_plan(&mut self, plan: ResourcePlan) {
        self.resources.insert(plan.resource_id.clone(), plan);
    }

    /// Returns true if the plan has no changes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.resources.values().all(|p| p.action == ResourceAction::NoOp)
    }
}

impl Default for Plan {
    fn default() -> Self {
        Self::new()
    }
}

/// A plan for a single resource.
#[derive(Debug, Clone)]
pub struct ResourcePlan {
    /// Resource identifier
    pub resource_id: String,

    /// Resource type (e.g., "aws_instance")
    pub resource_type: String,

    /// Action to take
    pub action: ResourceAction,

    /// Resource configuration
    pub config: serde_json::Value,
}

/// Action to take on a resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceAction {
    /// Create a new resource
    Create,
    /// Update an existing resource
    Update,
    /// Destroy the resource
    Destroy,
    /// No changes needed
    NoOp,
}

/// Result of applying a plan.
#[derive(Debug, Clone)]
pub struct ApplyResult {
    /// Results for each resource
    pub resources: std::collections::HashMap<String, ResourceResult>,
}

impl ApplyResult {
    /// Creates a new empty apply result.
    #[must_use]
    pub fn new() -> Self {
        Self {
            resources: std::collections::HashMap::new(),
        }
    }

    /// Adds a resource result.
    pub fn add_resource_result(&mut self, id: String, result: ResourceResult) {
        self.resources.insert(id, result);
    }

    /// Returns true if all resources were applied successfully.
    #[must_use]
    pub fn all_successful(&self) -> bool {
        self.resources.values().all(|r| r.success)
    }
}

impl Default for ApplyResult {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of applying changes to a single resource.
#[derive(Debug, Clone)]
pub struct ResourceResult {
    /// Resource identifier
    pub resource_id: String,

    /// Whether the operation succeeded
    pub success: bool,

    /// Optional error message
    pub message: Option<String>,
}
