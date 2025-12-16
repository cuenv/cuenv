//! IaC supervisor actor.
//!
//! The supervisor manages the lifecycle of resource actors, implementing
//! Erlang-style supervision with configurable restart policies.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef, SupervisionEvent};
use tracing::{debug, error, info, instrument, warn};

use super::resource::{ResourceActorMessage, ResourceState, StateNotification};

/// Messages for the IaC supervisor.
#[derive(Debug)]
pub enum SupervisorMessage {
    /// A resource actor has started
    ResourceStarted {
        /// Resource ID
        resource_id: String,
    },

    /// A resource actor has stopped
    ResourceStopped {
        /// Resource ID
        resource_id: String,
    },

    /// Get all managed resources (for debugging)
    GetManagedResources(ractor::RpcReplyPort<Vec<String>>),

    /// State notification from a resource actor
    ResourceStateChanged {
        /// Resource ID
        resource_id: String,
        /// Previous state
        previous: ResourceState,
        /// New state
        current: ResourceState,
    },

    /// Pause all operations
    Pause,

    /// Resume operations
    Resume,

    /// Shutdown all resources gracefully
    Shutdown,
}

/// Restart policy for supervised actors.
#[derive(Debug, Clone, Copy)]
pub enum RestartPolicy {
    /// Restart the actor on any failure
    Always,
    /// Never restart the actor
    Never,
    /// Restart up to N times within a time window
    Transient {
        /// Maximum restarts
        max_restarts: u32,
        /// Time window
        window: Duration,
    },
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self::Transient {
            max_restarts: 3,
            window: Duration::from_secs(60),
        }
    }
}

/// Information about a managed resource.
#[derive(Debug)]
struct ManagedResource {
    /// Resource ID
    resource_id: String,
    /// Actor reference (if running)
    actor: Option<ActorRef<ResourceActorMessage>>,
    /// Current state
    state: ResourceState,
    /// Restart count
    restart_count: u32,
    /// First restart in current window
    first_restart: Option<Instant>,
}

/// State for the IaC supervisor.
pub struct IacSupervisorState {
    /// Managed resources
    resources: HashMap<String, ManagedResource>,
    /// Maximum concurrent operations
    max_concurrent: usize,
    /// Currently running operations
    running_operations: usize,
    /// Whether operations are paused
    paused: bool,
    /// Restart policy
    restart_policy: RestartPolicy,
}

/// The IaC supervisor actor.
pub struct IacSupervisor {
    /// Maximum concurrent resource operations
    max_concurrent: usize,
}

impl IacSupervisor {
    /// Creates a new IaC supervisor.
    #[must_use]
    pub fn new(max_concurrent: usize) -> Self {
        Self { max_concurrent }
    }

    /// Checks if an actor should be restarted based on the restart policy.
    fn should_restart(state: &mut IacSupervisorState, resource_id: &str) -> bool {
        let resource = match state.resources.get_mut(resource_id) {
            Some(r) => r,
            None => return false,
        };

        match state.restart_policy {
            RestartPolicy::Always => true,
            RestartPolicy::Never => false,
            RestartPolicy::Transient { max_restarts, window } => {
                let now = Instant::now();

                // Reset window if expired
                if let Some(first) = resource.first_restart {
                    if now.duration_since(first) > window {
                        resource.restart_count = 0;
                        resource.first_restart = None;
                    }
                }

                if resource.restart_count < max_restarts {
                    if resource.first_restart.is_none() {
                        resource.first_restart = Some(now);
                    }
                    resource.restart_count += 1;
                    true
                } else {
                    false
                }
            }
        }
    }
}

#[async_trait]
impl Actor for IacSupervisor {
    type Msg = SupervisorMessage;
    type State = IacSupervisorState;
    type Arguments = ();

    #[instrument(name = "supervisor_pre_start", skip(self, _myself, _args))]
    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        _args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        info!(max_concurrent = self.max_concurrent, "IaC supervisor starting");

        Ok(IacSupervisorState {
            resources: HashMap::new(),
            max_concurrent: self.max_concurrent,
            running_operations: 0,
            paused: false,
            restart_policy: RestartPolicy::default(),
        })
    }

    #[instrument(name = "supervisor_handle", skip(self, _myself, state))]
    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        msg: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            SupervisorMessage::ResourceStarted { resource_id } => {
                debug!(resource_id = %resource_id, "Resource actor started");

                state.resources.insert(
                    resource_id.clone(),
                    ManagedResource {
                        resource_id,
                        actor: None,
                        state: ResourceState::Pending,
                        restart_count: 0,
                        first_restart: None,
                    },
                );
            }

            SupervisorMessage::ResourceStopped { resource_id } => {
                debug!(resource_id = %resource_id, "Resource actor stopped");

                if let Some(resource) = state.resources.get_mut(&resource_id) {
                    resource.actor = None;
                }
            }

            SupervisorMessage::GetManagedResources(reply) => {
                let resources: Vec<String> = state.resources.keys().cloned().collect();
                let _ = reply.send(resources);
            }

            SupervisorMessage::ResourceStateChanged {
                resource_id,
                previous,
                current,
            } => {
                debug!(
                    resource_id = %resource_id,
                    previous = %previous,
                    current = %current,
                    "Resource state changed"
                );

                if let Some(resource) = state.resources.get_mut(&resource_id) {
                    resource.state = current.clone();
                }

                // Update running operation count
                match (&previous, &current) {
                    (ResourceState::Pending, ResourceState::Creating { .. })
                    | (ResourceState::Created { .. }, ResourceState::Updating { .. })
                    | (ResourceState::Created { .. }, ResourceState::Deleting { .. }) => {
                        state.running_operations = state.running_operations.saturating_add(1);
                    }
                    (ResourceState::Creating { .. }, _)
                    | (ResourceState::Updating { .. }, _)
                    | (ResourceState::Deleting { .. }, _) => {
                        state.running_operations = state.running_operations.saturating_sub(1);
                    }
                    _ => {}
                }
            }

            SupervisorMessage::Pause => {
                info!("Pausing IaC operations");
                state.paused = true;
            }

            SupervisorMessage::Resume => {
                info!("Resuming IaC operations");
                state.paused = false;
            }

            SupervisorMessage::Shutdown => {
                info!("Shutting down IaC supervisor");
                // Signal all resources to stop
                for (resource_id, resource) in &state.resources {
                    if let Some(actor) = &resource.actor {
                        debug!(resource_id = %resource_id, "Stopping resource actor");
                        // Best effort - don't fail if actor is already stopped
                        let _ = actor.stop(None);
                    }
                }
            }
        }

        Ok(())
    }

    async fn handle_supervisor_evt(
        &self,
        _myself: ActorRef<Self::Msg>,
        event: SupervisionEvent,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match event {
            SupervisionEvent::ActorStarted(actor) => {
                debug!(actor_id = ?actor.get_id(), "Supervised actor started");
            }

            SupervisionEvent::ActorTerminated(actor, _final_state, reason) => {
                let actor_name = actor
                    .get_name()
                    .unwrap_or_else(|| format!("{:?}", actor.get_id()));

                // Extract resource ID from actor name (format: "resource-{id}")
                if let Some(resource_id) = actor_name.strip_prefix("resource-") {
                    warn!(
                        resource_id = %resource_id,
                        reason = ?reason,
                        "Resource actor terminated"
                    );

                    if Self::should_restart(state, resource_id) {
                        info!(
                            resource_id = %resource_id,
                            restart_count = state.resources.get(resource_id).map(|r| r.restart_count).unwrap_or(0),
                            "Restarting resource actor"
                        );
                        // TODO: Actually restart the actor
                        // This would require access to the original spawn arguments
                    } else {
                        error!(
                            resource_id = %resource_id,
                            "Resource actor exceeded restart limit, not restarting"
                        );
                    }
                }
            }

            SupervisionEvent::ActorFailed(actor, error) => {
                let actor_name = actor
                    .get_name()
                    .unwrap_or_else(|| format!("{:?}", actor.get_id()));

                error!(
                    actor = %actor_name,
                    error = %error,
                    "Supervised actor failed"
                );
            }

            SupervisionEvent::ProcessGroupChanged(_change) => {
                // Process group membership changed - used for pub/sub
            }
        }

        Ok(())
    }

    async fn post_stop(
        &self,
        _myself: ActorRef<Self::Msg>,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        info!("IaC supervisor stopped");
        Ok(())
    }
}

/// Extension for resource actors to receive state notifications.
impl Actor for StateNotificationReceiver {
    type Msg = StateNotification;
    type State = ();
    type Arguments = ();

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        _args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(())
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        msg: Self::Msg,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            StateNotification::StateChanged {
                resource_id,
                previous,
                current,
            } => {
                debug!(
                    resource_id = %resource_id,
                    previous = %previous,
                    current = %current,
                    "Received state notification"
                );
            }
        }
        Ok(())
    }
}

/// Actor that receives state notifications (for testing/debugging).
pub struct StateNotificationReceiver;

#[async_trait]
impl Actor for StateNotificationReceiver {
    type Msg = StateNotification;
    type State = ();
    type Arguments = ();

    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        _args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        Ok(())
    }

    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        msg: Self::Msg,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            StateNotification::StateChanged {
                resource_id,
                previous,
                current,
            } => {
                debug!(
                    resource_id = %resource_id,
                    previous = %previous,
                    current = %current,
                    "Received state notification"
                );
            }
        }
        Ok(())
    }
}
