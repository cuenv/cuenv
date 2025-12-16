//! Resource actor implementation.
//!
//! Each managed infrastructure resource is represented by a `ResourceActor`
//! that handles its lifecycle state machine and operations.

use std::sync::Arc;
use std::time::{Duration, Instant};

use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef, RpcReplyPort};
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, instrument, warn};

use crate::error::{Error, Result};
use crate::provider::ProviderClient;

use super::SupervisorMessage;

/// Messages that can be sent to a resource actor.
#[derive(Debug)]
pub enum ResourceActorMessage {
    /// Create the resource with the given configuration
    Create(serde_json::Value),

    /// Update the resource with the given configuration
    Update(serde_json::Value),

    /// Delete the resource
    Destroy,

    /// Refresh the resource state from the provider
    Refresh,

    /// Import an existing resource
    Import(String),

    /// Get the current resource state (RPC)
    GetState(RpcReplyPort<ResourceState>),

    /// Subscribe to state changes
    Subscribe(ActorRef<StateNotification>),

    /// Unsubscribe from state changes
    Unsubscribe(ActorRef<StateNotification>),

    /// Notify that a dependency is ready
    DependencyReady {
        /// Dependency resource ID
        dependency_id: String,
        /// Dependency state
        state: serde_json::Value,
    },

    /// Internal: Operation completed
    OperationComplete(OperationResult),
}

/// Notification sent to subscribers when state changes.
#[derive(Debug, Clone)]
pub enum StateNotification {
    /// Resource state changed
    StateChanged {
        /// Resource ID
        resource_id: String,
        /// Previous state
        previous: ResourceState,
        /// New state
        current: ResourceState,
    },
}

/// Result of a resource operation.
#[derive(Debug, Clone)]
pub struct OperationResult {
    /// Whether the operation succeeded
    pub success: bool,
    /// New state after operation
    pub new_state: Option<serde_json::Value>,
    /// Error message if failed
    pub error: Option<String>,
}

/// Resource lifecycle state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ResourceState {
    /// Resource is waiting for initialization
    Pending,

    /// Resource is waiting for dependencies
    WaitingForDependencies {
        /// List of pending dependency IDs
        pending: Vec<String>,
    },

    /// Resource is being created
    Creating {
        /// When creation started
        #[serde(with = "instant_serde")]
        started_at: Instant,
    },

    /// Resource has been created
    Created {
        /// Resource ID from provider
        resource_id: String,
        /// Current resource state
        state: serde_json::Value,
    },

    /// Resource is being updated
    Updating {
        /// Resource ID
        resource_id: String,
        /// Previous state
        previous_state: serde_json::Value,
        /// When update started
        #[serde(with = "instant_serde")]
        started_at: Instant,
    },

    /// Resource is being deleted
    Deleting {
        /// Resource ID
        resource_id: String,
        /// When deletion started
        #[serde(with = "instant_serde")]
        started_at: Instant,
    },

    /// Resource has been deleted
    Deleted,

    /// Resource is in an error state
    Error {
        /// Error message
        message: String,
        /// State before error occurred
        previous_state: Box<ResourceState>,
    },
}

/// Custom serialization for Instant (which doesn't implement Serialize)
mod instant_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::Instant;

    pub fn serialize<S: Serializer>(instant: &Instant, serializer: S) -> Result<S::Ok, S::Error> {
        instant.elapsed().as_secs().serialize(serializer)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(_deserializer: D) -> Result<Instant, D::Error> {
        // When deserializing, we just return now since we can't reconstruct the original instant
        Ok(Instant::now())
    }
}

impl ResourceState {
    /// Returns true if the resource is in a terminal state.
    #[must_use]
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Created { .. } | Self::Deleted | Self::Error { .. })
    }

    /// Returns true if the resource is ready (created successfully).
    #[must_use]
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Created { .. })
    }

    /// Returns the resource ID if available.
    #[must_use]
    pub fn resource_id(&self) -> Option<&str> {
        match self {
            Self::Created { resource_id, .. }
            | Self::Updating { resource_id, .. }
            | Self::Deleting { resource_id, .. } => Some(resource_id),
            _ => None,
        }
    }

    /// Returns the current state data if available.
    #[must_use]
    pub fn state_data(&self) -> Option<&serde_json::Value> {
        match self {
            Self::Created { state, .. } => Some(state),
            Self::Updating { previous_state, .. } => Some(previous_state),
            _ => None,
        }
    }
}

impl std::fmt::Display for ResourceState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "Pending"),
            Self::WaitingForDependencies { pending } => {
                write!(f, "WaitingForDependencies({} remaining)", pending.len())
            }
            Self::Creating { .. } => write!(f, "Creating"),
            Self::Created { resource_id, .. } => write!(f, "Created({resource_id})"),
            Self::Updating { resource_id, .. } => write!(f, "Updating({resource_id})"),
            Self::Deleting { resource_id, .. } => write!(f, "Deleting({resource_id})"),
            Self::Deleted => write!(f, "Deleted"),
            Self::Error { message, .. } => write!(f, "Error({message})"),
        }
    }
}

/// Internal state for the resource actor.
pub struct ResourceActorState {
    /// Current lifecycle state
    pub state: ResourceState,

    /// Desired configuration
    pub config: Option<serde_json::Value>,

    /// Subscribers for state changes
    pub subscribers: Vec<ActorRef<StateNotification>>,

    /// Dependencies that this resource is waiting for
    pub pending_dependencies: Vec<String>,

    /// Resolved dependency values
    pub dependency_values: std::collections::HashMap<String, serde_json::Value>,

    /// Private data from provider
    pub private_data: Vec<u8>,

    /// Pending operation (for async operations)
    pub pending_operation: Option<PendingOperation>,
}

/// A pending async operation.
#[derive(Debug, Clone)]
pub enum PendingOperation {
    Create,
    Update,
    Delete,
    Refresh,
    Import(String),
}

/// Resource actor that manages the lifecycle of a single infrastructure resource.
pub struct ResourceActor {
    /// Unique resource identifier
    resource_id: String,

    /// Resource type (e.g., "aws_instance")
    resource_type: String,

    /// Provider client
    provider: Arc<ProviderClient>,
}

impl ResourceActor {
    /// Creates a new resource actor.
    #[must_use]
    pub fn new(
        resource_id: String,
        resource_type: String,
        provider: Arc<ProviderClient>,
    ) -> Self {
        Self {
            resource_id,
            resource_type,
            provider,
        }
    }

    /// Notifies subscribers of a state change.
    fn notify_subscribers(
        state: &ResourceActorState,
        resource_id: &str,
        previous: ResourceState,
        current: ResourceState,
    ) {
        let notification = StateNotification::StateChanged {
            resource_id: resource_id.to_string(),
            previous,
            current,
        };

        for subscriber in &state.subscribers {
            if let Err(e) = subscriber.cast(notification.clone()) {
                warn!(error = %e, "Failed to notify subscriber");
            }
        }
    }

    /// Executes a create operation.
    async fn execute_create(
        &self,
        config: &serde_json::Value,
    ) -> OperationResult {
        info!(resource_id = %self.resource_id, "Creating resource");

        // Plan the change
        let plan_result = self
            .provider
            .plan_resource_change(&self.resource_type, None, config, config)
            .await;

        let planned = match plan_result {
            Ok(p) => p,
            Err(e) => {
                return OperationResult {
                    success: false,
                    new_state: None,
                    error: Some(e.to_string()),
                };
            }
        };

        // Apply the change
        let apply_result = self
            .provider
            .apply_resource_change(
                &self.resource_type,
                None,
                &planned.planned_state,
                config,
                planned.planned_private,
            )
            .await;

        match apply_result {
            Ok(result) => OperationResult {
                success: true,
                new_state: result.state,
                error: None,
            },
            Err(e) => OperationResult {
                success: false,
                new_state: None,
                error: Some(e.to_string()),
            },
        }
    }

    /// Executes an update operation.
    async fn execute_update(
        &self,
        current_state: &serde_json::Value,
        new_config: &serde_json::Value,
    ) -> OperationResult {
        info!(resource_id = %self.resource_id, "Updating resource");

        // Plan the change
        let plan_result = self
            .provider
            .plan_resource_change(&self.resource_type, Some(current_state), new_config, new_config)
            .await;

        let planned = match plan_result {
            Ok(p) => p,
            Err(e) => {
                return OperationResult {
                    success: false,
                    new_state: None,
                    error: Some(e.to_string()),
                };
            }
        };

        // Apply the change
        let apply_result = self
            .provider
            .apply_resource_change(
                &self.resource_type,
                Some(current_state),
                &planned.planned_state,
                new_config,
                planned.planned_private,
            )
            .await;

        match apply_result {
            Ok(result) => OperationResult {
                success: true,
                new_state: result.state,
                error: None,
            },
            Err(e) => OperationResult {
                success: false,
                new_state: None,
                error: Some(e.to_string()),
            },
        }
    }

    /// Executes a delete operation.
    async fn execute_delete(&self, current_state: &serde_json::Value) -> OperationResult {
        info!(resource_id = %self.resource_id, "Deleting resource");

        // Plan destruction (proposed state is null)
        let plan_result = self
            .provider
            .plan_resource_change(
                &self.resource_type,
                Some(current_state),
                &serde_json::Value::Null,
                &serde_json::Value::Null,
            )
            .await;

        let planned = match plan_result {
            Ok(p) => p,
            Err(e) => {
                return OperationResult {
                    success: false,
                    new_state: None,
                    error: Some(e.to_string()),
                };
            }
        };

        // Apply destruction
        let apply_result = self
            .provider
            .apply_resource_change(
                &self.resource_type,
                Some(current_state),
                &serde_json::Value::Null,
                &serde_json::Value::Null,
                planned.planned_private,
            )
            .await;

        match apply_result {
            Ok(_) => OperationResult {
                success: true,
                new_state: None,
                error: None,
            },
            Err(e) => OperationResult {
                success: false,
                new_state: Some(current_state.clone()),
                error: Some(e.to_string()),
            },
        }
    }

    /// Executes a refresh operation.
    async fn execute_refresh(&self, current_state: &serde_json::Value) -> OperationResult {
        info!(resource_id = %self.resource_id, "Refreshing resource");

        // Extract the ID from current state for the read
        let id = current_state
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or(&self.resource_id);

        match self.provider.read_resource(&self.resource_type, id).await {
            Ok(Some(state)) => OperationResult {
                success: true,
                new_state: Some(state),
                error: None,
            },
            Ok(None) => OperationResult {
                success: false,
                new_state: None,
                error: Some("Resource no longer exists".to_string()),
            },
            Err(e) => OperationResult {
                success: false,
                new_state: None,
                error: Some(e.to_string()),
            },
        }
    }
}

#[async_trait]
impl Actor for ResourceActor {
    type Msg = ResourceActorMessage;
    type State = ResourceActorState;
    type Arguments = ActorRef<SupervisorMessage>;

    #[instrument(name = "resource_actor_pre_start", skip(self, _myself, supervisor))]
    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        supervisor: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!(
            resource_id = %self.resource_id,
            resource_type = %self.resource_type,
            "Resource actor starting"
        );

        // Register with supervisor
        let _ = supervisor.cast(SupervisorMessage::ResourceStarted {
            resource_id: self.resource_id.clone(),
        });

        Ok(ResourceActorState {
            state: ResourceState::Pending,
            config: None,
            subscribers: Vec::new(),
            pending_dependencies: Vec::new(),
            dependency_values: std::collections::HashMap::new(),
            private_data: Vec::new(),
            pending_operation: None,
        })
    }

    #[instrument(name = "resource_actor_handle", skip(self, myself, state))]
    async fn handle(
        &self,
        myself: ActorRef<Self::Msg>,
        msg: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            ResourceActorMessage::Create(config) => {
                let previous = state.state.clone();
                state.config = Some(config.clone());

                // Check if we have pending dependencies
                if !state.pending_dependencies.is_empty() {
                    state.state = ResourceState::WaitingForDependencies {
                        pending: state.pending_dependencies.clone(),
                    };
                    Self::notify_subscribers(state, &self.resource_id, previous, state.state.clone());
                    return Ok(());
                }

                state.state = ResourceState::Creating {
                    started_at: Instant::now(),
                };
                state.pending_operation = Some(PendingOperation::Create);
                Self::notify_subscribers(state, &self.resource_id, previous, state.state.clone());

                // Spawn async create operation
                let provider = Arc::clone(&self.provider);
                let resource_type = self.resource_type.clone();
                let actor_ref = myself.clone();

                tokio::spawn(async move {
                    let result = Self::execute_create_static(&provider, &resource_type, &config).await;
                    let _ = actor_ref.cast(ResourceActorMessage::OperationComplete(result));
                });
            }

            ResourceActorMessage::Update(config) => {
                let previous = state.state.clone();

                match &state.state {
                    ResourceState::Created { resource_id, state: current_state } => {
                        let rid = resource_id.clone();
                        let prev_state = current_state.clone();

                        state.config = Some(config.clone());
                        state.state = ResourceState::Updating {
                            resource_id: rid,
                            previous_state: prev_state.clone(),
                            started_at: Instant::now(),
                        };
                        state.pending_operation = Some(PendingOperation::Update);
                        Self::notify_subscribers(state, &self.resource_id, previous, state.state.clone());

                        // Spawn async update operation
                        let provider = Arc::clone(&self.provider);
                        let resource_type = self.resource_type.clone();
                        let actor_ref = myself.clone();

                        tokio::spawn(async move {
                            let result = Self::execute_update_static(&provider, &resource_type, &prev_state, &config).await;
                            let _ = actor_ref.cast(ResourceActorMessage::OperationComplete(result));
                        });
                    }
                    _ => {
                        warn!(
                            resource_id = %self.resource_id,
                            state = %state.state,
                            "Cannot update resource in current state"
                        );
                    }
                }
            }

            ResourceActorMessage::Destroy => {
                let previous = state.state.clone();

                match &state.state {
                    ResourceState::Created { resource_id, state: current_state } => {
                        let rid = resource_id.clone();
                        let curr_state = current_state.clone();

                        state.state = ResourceState::Deleting {
                            resource_id: rid,
                            started_at: Instant::now(),
                        };
                        state.pending_operation = Some(PendingOperation::Delete);
                        Self::notify_subscribers(state, &self.resource_id, previous, state.state.clone());

                        // Spawn async delete operation
                        let provider = Arc::clone(&self.provider);
                        let resource_type = self.resource_type.clone();
                        let actor_ref = myself.clone();

                        tokio::spawn(async move {
                            let result = Self::execute_delete_static(&provider, &resource_type, &curr_state).await;
                            let _ = actor_ref.cast(ResourceActorMessage::OperationComplete(result));
                        });
                    }
                    ResourceState::Pending | ResourceState::WaitingForDependencies { .. } => {
                        // Resource never created, just mark as deleted
                        state.state = ResourceState::Deleted;
                        Self::notify_subscribers(state, &self.resource_id, previous, state.state.clone());
                    }
                    _ => {
                        warn!(
                            resource_id = %self.resource_id,
                            state = %state.state,
                            "Cannot destroy resource in current state"
                        );
                    }
                }
            }

            ResourceActorMessage::Refresh => {
                if let ResourceState::Created { state: current_state, .. } = &state.state {
                    let curr_state = current_state.clone();
                    state.pending_operation = Some(PendingOperation::Refresh);

                    // Spawn async refresh operation
                    let provider = Arc::clone(&self.provider);
                    let resource_type = self.resource_type.clone();
                    let actor_ref = myself.clone();

                    tokio::spawn(async move {
                        let result = Self::execute_refresh_static(&provider, &resource_type, &curr_state).await;
                        let _ = actor_ref.cast(ResourceActorMessage::OperationComplete(result));
                    });
                }
            }

            ResourceActorMessage::Import(id) => {
                let previous = state.state.clone();
                state.state = ResourceState::Creating {
                    started_at: Instant::now(),
                };
                state.pending_operation = Some(PendingOperation::Import(id.clone()));
                Self::notify_subscribers(state, &self.resource_id, previous, state.state.clone());

                // Spawn async import operation
                let provider = Arc::clone(&self.provider);
                let resource_type = self.resource_type.clone();
                let actor_ref = myself.clone();

                tokio::spawn(async move {
                    let result = match provider.import_resource(&resource_type, &id).await {
                        Ok(resources) if !resources.is_empty() => OperationResult {
                            success: true,
                            new_state: Some(resources[0].state.clone()),
                            error: None,
                        },
                        Ok(_) => OperationResult {
                            success: false,
                            new_state: None,
                            error: Some("No resources imported".to_string()),
                        },
                        Err(e) => OperationResult {
                            success: false,
                            new_state: None,
                            error: Some(e.to_string()),
                        },
                    };
                    let _ = actor_ref.cast(ResourceActorMessage::OperationComplete(result));
                });
            }

            ResourceActorMessage::GetState(reply) => {
                let _ = reply.send(state.state.clone());
            }

            ResourceActorMessage::Subscribe(subscriber) => {
                if !state.subscribers.iter().any(|s| s.get_id() == subscriber.get_id()) {
                    state.subscribers.push(subscriber);
                }
            }

            ResourceActorMessage::Unsubscribe(subscriber) => {
                state.subscribers.retain(|s| s.get_id() != subscriber.get_id());
            }

            ResourceActorMessage::DependencyReady { dependency_id, state: dep_state } => {
                state.dependency_values.insert(dependency_id.clone(), dep_state);
                state.pending_dependencies.retain(|d| d != &dependency_id);

                // Check if all dependencies are now ready
                if state.pending_dependencies.is_empty() {
                    if let (Some(config), ResourceState::WaitingForDependencies { .. }) =
                        (&state.config, &state.state)
                    {
                        let previous = state.state.clone();
                        state.state = ResourceState::Creating {
                            started_at: Instant::now(),
                        };
                        state.pending_operation = Some(PendingOperation::Create);
                        Self::notify_subscribers(state, &self.resource_id, previous, state.state.clone());

                        // Now execute create
                        let provider = Arc::clone(&self.provider);
                        let resource_type = self.resource_type.clone();
                        let config = config.clone();
                        let actor_ref = myself.clone();

                        tokio::spawn(async move {
                            let result = Self::execute_create_static(&provider, &resource_type, &config).await;
                            let _ = actor_ref.cast(ResourceActorMessage::OperationComplete(result));
                        });
                    }
                }
            }

            ResourceActorMessage::OperationComplete(result) => {
                let previous = state.state.clone();
                state.pending_operation = None;

                if result.success {
                    match &previous {
                        ResourceState::Creating { .. } => {
                            if let Some(new_state) = result.new_state {
                                let id = new_state
                                    .get("id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or(&self.resource_id)
                                    .to_string();

                                state.state = ResourceState::Created {
                                    resource_id: id,
                                    state: new_state,
                                };
                                info!(resource_id = %self.resource_id, "Resource created successfully");
                            }
                        }
                        ResourceState::Updating { .. } => {
                            if let Some(new_state) = result.new_state {
                                let id = new_state
                                    .get("id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or(&self.resource_id)
                                    .to_string();

                                state.state = ResourceState::Created {
                                    resource_id: id,
                                    state: new_state,
                                };
                                info!(resource_id = %self.resource_id, "Resource updated successfully");
                            }
                        }
                        ResourceState::Deleting { .. } => {
                            state.state = ResourceState::Deleted;
                            info!(resource_id = %self.resource_id, "Resource deleted successfully");
                        }
                        _ => {}
                    }
                } else {
                    let error_msg = result.error.unwrap_or_else(|| "Unknown error".to_string());
                    error!(resource_id = %self.resource_id, error = %error_msg, "Operation failed");

                    state.state = ResourceState::Error {
                        message: error_msg,
                        previous_state: Box::new(previous.clone()),
                    };
                }

                Self::notify_subscribers(state, &self.resource_id, previous, state.state.clone());
            }
        }

        Ok(())
    }

    async fn post_stop(
        &self,
        _myself: ActorRef<Self::Msg>,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        debug!(resource_id = %self.resource_id, "Resource actor stopped");
        Ok(())
    }
}

// Static methods for spawned tasks (can't use &self in tokio::spawn)
impl ResourceActor {
    async fn execute_create_static(
        provider: &ProviderClient,
        resource_type: &str,
        config: &serde_json::Value,
    ) -> OperationResult {
        // Plan the change
        let plan_result = provider
            .plan_resource_change(resource_type, None, config, config)
            .await;

        let planned = match plan_result {
            Ok(p) => p,
            Err(e) => {
                return OperationResult {
                    success: false,
                    new_state: None,
                    error: Some(e.to_string()),
                };
            }
        };

        // Apply the change
        match provider
            .apply_resource_change(
                resource_type,
                None,
                &planned.planned_state,
                config,
                planned.planned_private,
            )
            .await
        {
            Ok(result) => OperationResult {
                success: true,
                new_state: result.state,
                error: None,
            },
            Err(e) => OperationResult {
                success: false,
                new_state: None,
                error: Some(e.to_string()),
            },
        }
    }

    async fn execute_update_static(
        provider: &ProviderClient,
        resource_type: &str,
        current_state: &serde_json::Value,
        new_config: &serde_json::Value,
    ) -> OperationResult {
        let plan_result = provider
            .plan_resource_change(resource_type, Some(current_state), new_config, new_config)
            .await;

        let planned = match plan_result {
            Ok(p) => p,
            Err(e) => {
                return OperationResult {
                    success: false,
                    new_state: None,
                    error: Some(e.to_string()),
                };
            }
        };

        match provider
            .apply_resource_change(
                resource_type,
                Some(current_state),
                &planned.planned_state,
                new_config,
                planned.planned_private,
            )
            .await
        {
            Ok(result) => OperationResult {
                success: true,
                new_state: result.state,
                error: None,
            },
            Err(e) => OperationResult {
                success: false,
                new_state: None,
                error: Some(e.to_string()),
            },
        }
    }

    async fn execute_delete_static(
        provider: &ProviderClient,
        resource_type: &str,
        current_state: &serde_json::Value,
    ) -> OperationResult {
        let plan_result = provider
            .plan_resource_change(
                resource_type,
                Some(current_state),
                &serde_json::Value::Null,
                &serde_json::Value::Null,
            )
            .await;

        let planned = match plan_result {
            Ok(p) => p,
            Err(e) => {
                return OperationResult {
                    success: false,
                    new_state: None,
                    error: Some(e.to_string()),
                };
            }
        };

        match provider
            .apply_resource_change(
                resource_type,
                Some(current_state),
                &serde_json::Value::Null,
                &serde_json::Value::Null,
                planned.planned_private,
            )
            .await
        {
            Ok(_) => OperationResult {
                success: true,
                new_state: None,
                error: None,
            },
            Err(e) => OperationResult {
                success: false,
                new_state: Some(current_state.clone()),
                error: Some(e.to_string()),
            },
        }
    }

    async fn execute_refresh_static(
        provider: &ProviderClient,
        resource_type: &str,
        current_state: &serde_json::Value,
    ) -> OperationResult {
        let id = current_state
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        match provider.read_resource(resource_type, id).await {
            Ok(Some(state)) => OperationResult {
                success: true,
                new_state: Some(state),
                error: None,
            },
            Ok(None) => OperationResult {
                success: false,
                new_state: None,
                error: Some("Resource no longer exists".to_string()),
            },
            Err(e) => OperationResult {
                success: false,
                new_state: None,
                error: Some(e.to_string()),
            },
        }
    }
}
