//! Actor-based resource orchestration using Ractor.
//!
//! This module implements the actor system for managing infrastructure resources
//! with strict ordering guarantees and dependency management.
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                     IacSupervisor                           │
//! │  (Manages resource actor lifecycle and restart policies)   │
//! ├─────────────────────────────────────────────────────────────┤
//! │              DependencyManagerActor                         │
//! │  (Tracks dependencies, notifies when dependencies ready)   │
//! ├─────────────────────────────────────────────────────────────┤
//! │                    ResourceActors                           │
//! │  (One per resource, handles lifecycle state machine)       │
//! └─────────────────────────────────────────────────────────────┘
//! ```

mod dependency;
mod resource;
mod supervisor;

pub use dependency::{DependencyManagerActor, DependencyMessage};
pub use resource::{ResourceActor, ResourceActorMessage, ResourceState};
pub use supervisor::{IacSupervisor, SupervisorMessage};

use std::sync::Arc;

use ractor::{Actor, ActorRef};
use tracing::instrument;

use crate::error::Result;
use crate::provider::ProviderClient;

/// Starts the IaC supervisor actor.
///
/// # Arguments
///
/// * `max_concurrent` - Maximum number of concurrent resource operations
///
/// # Errors
///
/// Returns an error if the supervisor cannot be started.
#[instrument(name = "start_supervisor")]
pub async fn start_supervisor(
    max_concurrent: usize,
) -> Result<(ActorRef<SupervisorMessage>, ractor::ActorCell)> {
    let (actor, handle) = Actor::spawn(
        Some("iac-supervisor".to_string()),
        IacSupervisor::new(max_concurrent),
        (),
    )
    .await?;

    Ok((actor, handle))
}

/// Starts a resource actor.
///
/// # Arguments
///
/// * `resource_id` - Unique resource identifier
/// * `resource_type` - Resource type (e.g., "aws_instance")
/// * `provider` - Provider client for this resource
/// * `supervisor` - Supervisor actor reference
///
/// # Errors
///
/// Returns an error if the actor cannot be started.
#[instrument(name = "start_resource_actor", skip(provider, supervisor))]
pub async fn start_resource_actor(
    resource_id: String,
    resource_type: String,
    provider: Arc<ProviderClient>,
    supervisor: ActorRef<SupervisorMessage>,
) -> Result<(ActorRef<ResourceActorMessage>, ractor::ActorCell)> {
    let actor_name = format!("resource-{resource_id}");

    let (actor, handle) = Actor::spawn(
        Some(actor_name),
        ResourceActor::new(resource_id, resource_type, provider),
        supervisor,
    )
    .await?;

    Ok((actor, handle))
}

/// Starts the dependency manager actor.
///
/// # Errors
///
/// Returns an error if the actor cannot be started.
#[instrument(name = "start_dependency_manager")]
pub async fn start_dependency_manager(
) -> Result<(ActorRef<DependencyMessage>, ractor::ActorCell)> {
    let (actor, handle) = Actor::spawn(
        Some("dependency-manager".to_string()),
        DependencyManagerActor::new(),
        (),
    )
    .await?;

    Ok((actor, handle))
}
