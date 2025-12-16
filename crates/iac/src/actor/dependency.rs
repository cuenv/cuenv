//! Dependency manager actor.
//!
//! Tracks resource dependencies and notifies dependent resources when their
//! dependencies become ready.

use std::collections::{HashMap, HashSet};

use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::visit::EdgeRef;
use ractor::{async_trait, Actor, ActorProcessingErr, ActorRef};
use tracing::{debug, instrument, warn};

use super::resource::{ResourceActorMessage, ResourceState, StateNotification};

/// Messages for the dependency manager.
#[derive(Debug)]
pub enum DependencyMessage {
    /// Register a resource with its dependencies
    Register {
        /// Resource ID
        resource_id: String,
        /// Actor reference
        actor: ActorRef<ResourceActorMessage>,
        /// List of dependency resource IDs
        dependencies: Vec<String>,
    },

    /// Unregister a resource
    Unregister {
        /// Resource ID
        resource_id: String,
    },

    /// A resource's state has changed
    ResourceStateChanged {
        /// Resource ID
        resource_id: String,
        /// New state
        state: ResourceState,
    },

    /// Get the dependency graph (for debugging)
    GetDependencyGraph(ractor::RpcReplyPort<Vec<(String, Vec<String>)>>),

    /// Check if all dependencies for a resource are ready
    CheckDependencies {
        /// Resource ID
        resource_id: String,
        /// Reply port
        reply: ractor::RpcReplyPort<DependencyCheckResult>,
    },

    /// Get resources that depend on a given resource
    GetDependents {
        /// Resource ID
        resource_id: String,
        /// Reply port
        reply: ractor::RpcReplyPort<Vec<String>>,
    },
}

/// Result of checking dependencies.
#[derive(Debug, Clone)]
pub struct DependencyCheckResult {
    /// Whether all dependencies are ready
    pub all_ready: bool,
    /// List of pending dependencies
    pub pending: Vec<String>,
    /// Resolved dependency values
    pub values: HashMap<String, serde_json::Value>,
}

/// Information about a registered resource.
#[derive(Debug)]
struct RegisteredResource {
    /// Actor reference
    actor: ActorRef<ResourceActorMessage>,
    /// Current state
    state: ResourceState,
    /// Node index in dependency graph
    node_index: NodeIndex,
}

/// State for the dependency manager.
pub struct DependencyManagerState {
    /// Registered resources
    resources: HashMap<String, RegisteredResource>,
    /// Dependency graph (edges point from dependency to dependent)
    graph: DiGraph<String, ()>,
    /// Mapping from resource ID to node index
    node_indices: HashMap<String, NodeIndex>,
}

impl DependencyManagerState {
    fn new() -> Self {
        Self {
            resources: HashMap::new(),
            graph: DiGraph::new(),
            node_indices: HashMap::new(),
        }
    }

    /// Gets or creates a node index for a resource ID.
    fn get_or_create_node(&mut self, resource_id: &str) -> NodeIndex {
        if let Some(&idx) = self.node_indices.get(resource_id) {
            idx
        } else {
            let idx = self.graph.add_node(resource_id.to_string());
            self.node_indices.insert(resource_id.to_string(), idx);
            idx
        }
    }

    /// Gets all resources that directly depend on the given resource.
    fn get_dependents(&self, resource_id: &str) -> Vec<String> {
        if let Some(&node_idx) = self.node_indices.get(resource_id) {
            self.graph
                .edges(node_idx)
                .map(|edge| self.graph[edge.target()].clone())
                .collect()
        } else {
            Vec::new()
        }
    }

    /// Gets all dependencies of a resource.
    fn get_dependencies(&self, resource_id: &str) -> Vec<String> {
        if let Some(&node_idx) = self.node_indices.get(resource_id) {
            self.graph
                .edges_directed(node_idx, petgraph::Direction::Incoming)
                .map(|edge| self.graph[edge.source()].clone())
                .collect()
        } else {
            Vec::new()
        }
    }

    /// Checks if all dependencies of a resource are ready.
    fn check_dependencies(&self, resource_id: &str) -> DependencyCheckResult {
        let dependencies = self.get_dependencies(resource_id);
        let mut pending = Vec::new();
        let mut values = HashMap::new();

        for dep_id in &dependencies {
            if let Some(resource) = self.resources.get(dep_id) {
                if resource.state.is_ready() {
                    if let Some(state_data) = resource.state.state_data() {
                        values.insert(dep_id.clone(), state_data.clone());
                    }
                } else {
                    pending.push(dep_id.clone());
                }
            } else {
                pending.push(dep_id.clone());
            }
        }

        DependencyCheckResult {
            all_ready: pending.is_empty(),
            pending,
            values,
        }
    }
}

/// Actor that manages resource dependencies.
pub struct DependencyManagerActor;

impl DependencyManagerActor {
    /// Creates a new dependency manager actor.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for DependencyManagerActor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Actor for DependencyManagerActor {
    type Msg = DependencyMessage;
    type State = DependencyManagerState;
    type Arguments = ();

    #[instrument(name = "dependency_manager_pre_start", skip(self, _myself, _args))]
    async fn pre_start(
        &self,
        _myself: ActorRef<Self::Msg>,
        _args: Self::Arguments,
    ) -> Result<Self::State, ActorProcessingErr> {
        debug!("Dependency manager starting");
        Ok(DependencyManagerState::new())
    }

    #[instrument(name = "dependency_manager_handle", skip(self, _myself, state))]
    async fn handle(
        &self,
        _myself: ActorRef<Self::Msg>,
        msg: Self::Msg,
        state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        match msg {
            DependencyMessage::Register {
                resource_id,
                actor,
                dependencies,
            } => {
                debug!(
                    resource_id = %resource_id,
                    dependencies = ?dependencies,
                    "Registering resource"
                );

                // Get or create node for this resource
                let node_idx = state.get_or_create_node(&resource_id);

                // Add edges from dependencies to this resource
                for dep_id in &dependencies {
                    let dep_idx = state.get_or_create_node(dep_id);
                    // Edge from dependency to dependent
                    if !state.graph.contains_edge(dep_idx, node_idx) {
                        state.graph.add_edge(dep_idx, node_idx, ());
                    }
                }

                // Store the resource
                state.resources.insert(
                    resource_id.clone(),
                    RegisteredResource {
                        actor,
                        state: ResourceState::Pending,
                        node_index: node_idx,
                    },
                );

                // Check if dependencies are already satisfied
                let check = state.check_dependencies(&resource_id);
                if !check.all_ready && !check.pending.is_empty() {
                    debug!(
                        resource_id = %resource_id,
                        pending = ?check.pending,
                        "Resource waiting for dependencies"
                    );
                }
            }

            DependencyMessage::Unregister { resource_id } => {
                debug!(resource_id = %resource_id, "Unregistering resource");

                if let Some(_resource) = state.resources.remove(&resource_id) {
                    // Note: We don't remove the node from the graph to maintain
                    // structural integrity. The node will just have no associated actor.
                }
            }

            DependencyMessage::ResourceStateChanged { resource_id, state: new_state } => {
                debug!(
                    resource_id = %resource_id,
                    state = %new_state,
                    "Resource state changed"
                );

                let was_ready = state
                    .resources
                    .get(&resource_id)
                    .map(|r| r.state.is_ready())
                    .unwrap_or(false);

                // Update state
                if let Some(resource) = state.resources.get_mut(&resource_id) {
                    resource.state = new_state.clone();
                }

                // If resource just became ready, notify dependents
                if new_state.is_ready() && !was_ready {
                    let dependents = state.get_dependents(&resource_id);

                    for dependent_id in dependents {
                        if let Some(dependent) = state.resources.get(&dependent_id) {
                            // Get the state data to send
                            let state_data = new_state
                                .state_data()
                                .cloned()
                                .unwrap_or(serde_json::Value::Null);

                            debug!(
                                dependent = %dependent_id,
                                dependency = %resource_id,
                                "Notifying dependent of ready dependency"
                            );

                            if let Err(e) = dependent.actor.cast(ResourceActorMessage::DependencyReady {
                                dependency_id: resource_id.clone(),
                                state: state_data,
                            }) {
                                warn!(
                                    dependent = %dependent_id,
                                    error = %e,
                                    "Failed to notify dependent"
                                );
                            }
                        }
                    }
                }
            }

            DependencyMessage::GetDependencyGraph(reply) => {
                let graph: Vec<(String, Vec<String>)> = state
                    .resources
                    .keys()
                    .map(|id| {
                        let deps = state.get_dependencies(id);
                        (id.clone(), deps)
                    })
                    .collect();

                let _ = reply.send(graph);
            }

            DependencyMessage::CheckDependencies { resource_id, reply } => {
                let result = state.check_dependencies(&resource_id);
                let _ = reply.send(result);
            }

            DependencyMessage::GetDependents { resource_id, reply } => {
                let dependents = state.get_dependents(&resource_id);
                let _ = reply.send(dependents);
            }
        }

        Ok(())
    }

    async fn post_stop(
        &self,
        _myself: ActorRef<Self::Msg>,
        _state: &mut Self::State,
    ) -> Result<(), ActorProcessingErr> {
        debug!("Dependency manager stopped");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dependency_graph() {
        let mut state = DependencyManagerState::new();

        // Create nodes
        let vpc_idx = state.get_or_create_node("vpc");
        let subnet_idx = state.get_or_create_node("subnet");
        let instance_idx = state.get_or_create_node("instance");

        // subnet depends on vpc
        state.graph.add_edge(vpc_idx, subnet_idx, ());
        // instance depends on subnet
        state.graph.add_edge(subnet_idx, instance_idx, ());

        // Check dependents
        assert_eq!(state.get_dependents("vpc"), vec!["subnet".to_string()]);
        assert_eq!(state.get_dependents("subnet"), vec!["instance".to_string()]);
        assert!(state.get_dependents("instance").is_empty());

        // Check dependencies
        assert!(state.get_dependencies("vpc").is_empty());
        assert_eq!(state.get_dependencies("subnet"), vec!["vpc".to_string()]);
        assert_eq!(state.get_dependencies("instance"), vec!["subnet".to_string()]);
    }
}
