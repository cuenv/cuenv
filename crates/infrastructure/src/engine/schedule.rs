//! The apply schedule: one dependency graph of operation nodes.
//!
//! Every change that does work becomes one node, except a replacement, which
//! is two: the delete of the old object and the create of the new one. The
//! edges follow Terraform's `DestroyEdgeTransformer`, "A depends on B" meaning
//! A runs after B:
//!
//! * a create or update of R depends on the create or update of every
//!   resource R is configured to depend on;
//! * the create half of a replacement depends on its delete half;
//! * the delete of D depends on the delete of every resource whose stored
//!   record depends on D (dependents go before what they depend on);
//! * a create or update of R depends on the delete of every resource whose
//!   stored record depends on R (Terraform's `creators` edge: an object that
//!   hangs off R is gone before R is changed).
//!
//! Two further edges are only added while they cannot form a cycle, so they
//! never make an ordering impossible:
//!
//! * detachment: the delete of D waits for the in-place update (or refresh)
//!   of a resource whose stored record depends on D but whose configuration
//!   no longer does, so the dependent lets go of D first;
//! * safety: the delete half of a replacement waits for the creates and
//!   updates of everything its replacement is configured to depend on, so a
//!   failed prerequisite is known before the old object is destroyed.
//!
//! Among the nodes that may run, deletes go first, then refreshes, then
//! creates and updates, each group in dependency order; the create half of a
//! replacement follows its delete as soon as it may. The resulting order is
//! the apply order, and the plan lists its changes in the order each one
//! first appears in it.

use std::collections::{BTreeMap, BTreeSet};

use crate::error::{ChangeFailure, InfrastructureError, Result};
use crate::state::{RecordVersion, ResourceAddress};

use super::{Action, ApplyEvent, ApplyStep, ResourceChange, StepKind, topological_order};

/// What a node does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OperationKind {
    /// Rewrite the stored record with refreshed state.
    Refresh,
    /// Delete a resource that is no longer declared.
    Delete,
    /// Create or update a resource.
    Apply,
    /// The delete half of a replacement.
    ReplaceDelete,
    /// The create half of a replacement.
    ReplaceCreate,
}

impl OperationKind {
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Refresh => "refresh",
            Self::Delete | Self::ReplaceDelete => "delete",
            Self::Apply => "apply",
            Self::ReplaceCreate => "create",
        }
    }

    /// Deletes go first, then refreshes, then creates and updates.
    const fn priority(self) -> u8 {
        match self {
            Self::Delete | Self::ReplaceDelete => 0,
            Self::Refresh => 1,
            Self::Apply | Self::ReplaceCreate => 2,
        }
    }
}

/// One phase of a resource change, borrowing the exact provider plan bytes.
#[derive(Debug)]
pub(super) struct ApplyOperation<'plan> {
    pub(super) change: &'plan ResourceChange,
    pub(super) steps: &'plan [ApplyStep],
    pub(super) expected: RecordVersion,
    pub(super) kind: OperationKind,
}

impl ApplyOperation<'_> {
    /// Whether this operation is the first thing done for its change.
    pub(super) const fn starts_change(&self) -> bool {
        !matches!(self.kind, OperationKind::ReplaceCreate)
    }

    /// Whether the change is complete once this operation is.
    pub(super) const fn completes_change(&self) -> bool {
        !matches!(self.kind, OperationKind::ReplaceDelete)
    }
}

/// A node of the schedule, with its place in the dependency graph.
#[derive(Debug)]
pub(super) struct ScheduledOperation<'plan> {
    pub(super) operation: ApplyOperation<'plan>,
    /// Nodes that cannot start until this one finishes.
    pub(super) successors: Vec<usize>,
    /// The other half of a replacement.
    pub(super) counterpart: Option<usize>,
}

/// Every operation of a plan, in apply order.
#[derive(Debug)]
pub(super) struct Schedule<'plan> {
    pub(super) operations: Vec<ScheduledOperation<'plan>>,
    /// Indices into the changes the schedule was built from: every change in
    /// the order its first operation runs, changes without work last.
    pub(super) change_order: Vec<usize>,
}

impl<'plan> Schedule<'plan> {
    /// Build the schedule of `changes`.
    ///
    /// The result depends only on what the changes contain, never on their
    /// order in the slice.
    ///
    /// # Errors
    ///
    /// Returns a configuration error for a malformed replacement or for
    /// operations that depend on each other in a cycle.
    pub(super) fn build(changes: &'plan [ResourceChange]) -> Result<Self> {
        validate_replacements(changes)?;
        let mut graph = Graph::new(changes);
        graph.add_operations();
        graph.add_required_edges();
        graph.add_preferred_edges();
        graph.into_schedule()
    }
}

fn validate_replacements(changes: &[ResourceChange]) -> Result<()> {
    for change in changes
        .iter()
        .filter(|change| change.action == Action::Replace)
    {
        if change.stored.is_none()
            || change.steps.len() != 2
            || change.steps[0].kind != StepKind::Delete
            || change.steps[1].kind != StepKind::Create
        {
            return Err(InfrastructureError::configuration(format!(
                "invalid replacement steps for {}",
                change.address
            )));
        }
    }
    Ok(())
}

/// Turns dependency names, as stored and as configured, into addresses.
///
/// Dependencies are recorded as full `type.name` addresses. Bare names (the
/// shape records used to have) are resolved against the stored records of
/// the tenant, or, for configured dependencies, against every resource the
/// plan knows.
struct Dependencies {
    /// For every change, the addresses its stored record depends on.
    stored: Vec<BTreeSet<String>>,
    /// For every change, the addresses its configuration depends on.
    configured: Vec<BTreeSet<String>>,
}

impl Dependencies {
    fn new(changes: &[ResourceChange]) -> Self {
        let mut stored_addresses = BTreeSet::new();
        let mut stored_by_name: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut known_addresses = BTreeSet::new();
        let mut known_by_name: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for change in changes {
            let address = change.address.to_string();
            known_by_name
                .entry(change.address.name.clone())
                .or_default()
                .push(address.clone());
            known_addresses.insert(address.clone());
            if change.stored.is_some() {
                stored_by_name
                    .entry(change.address.name.clone())
                    .or_default()
                    .push(address.clone());
                stored_addresses.insert(address);
            }
        }
        Self {
            stored: changes
                .iter()
                .map(|change| {
                    change.stored.as_ref().map_or_else(BTreeSet::new, |stored| {
                        resolve(&stored.dependencies, &stored_addresses, &stored_by_name)
                    })
                })
                .collect(),
            configured: changes
                .iter()
                .map(|change| resolve(&change.dependencies, &known_addresses, &known_by_name))
                .collect(),
        }
    }
}

fn resolve(
    dependencies: &[String],
    addresses: &BTreeSet<String>,
    by_name: &BTreeMap<String, Vec<String>>,
) -> BTreeSet<String> {
    let mut resolved = BTreeSet::new();
    for dependency in dependencies {
        if dependency.contains('.') {
            if addresses.contains(dependency) {
                resolved.insert(dependency.clone());
            }
        } else if let Some(candidates) = by_name.get(dependency) {
            resolved.extend(candidates.iter().cloned());
        }
    }
    resolved
}

struct RawOperation<'plan> {
    operation: ApplyOperation<'plan>,
    /// Position of the change in the slice the schedule is built from.
    change: usize,
    counterpart: Option<usize>,
}

/// The graph under construction. Edges point from a node to the nodes it
/// waits for.
struct Graph<'plan> {
    changes: &'plan [ResourceChange],
    dependencies: Dependencies,
    operations: Vec<RawOperation<'plan>>,
    waits_for: Vec<BTreeSet<usize>>,
    waited_on_by: Vec<BTreeSet<usize>>,
    /// The delete of a resource: an orphan delete or a replacement's first half.
    delete_of: BTreeMap<String, usize>,
    /// The create or update of a resource: a replacement's second half, an
    /// ordinary create or an in-place update.
    apply_of: BTreeMap<String, usize>,
    /// The record rewrite of an unchanged resource.
    refresh_of: BTreeMap<String, usize>,
}

impl<'plan> Graph<'plan> {
    fn new(changes: &'plan [ResourceChange]) -> Self {
        Self {
            changes,
            dependencies: Dependencies::new(changes),
            operations: Vec::new(),
            waits_for: Vec::new(),
            waited_on_by: Vec::new(),
            delete_of: BTreeMap::new(),
            apply_of: BTreeMap::new(),
            refresh_of: BTreeMap::new(),
        }
    }

    fn push(&mut self, operation: RawOperation<'plan>) -> usize {
        self.operations.push(operation);
        self.waits_for.push(BTreeSet::new());
        self.waited_on_by.push(BTreeSet::new());
        self.operations.len() - 1
    }

    fn add_operations(&mut self) {
        let mut order: Vec<usize> = (0..self.changes.len()).collect();
        order.sort_by_key(|index| self.changes[*index].address.to_string());
        for index in order {
            let change = &self.changes[index];
            let address = change.address.to_string();
            let stored = RecordVersion::of(change.stored.as_ref());
            let single = |kind| RawOperation {
                operation: ApplyOperation {
                    change,
                    steps: &change.steps,
                    expected: stored,
                    kind,
                },
                change: index,
                counterpart: None,
            };
            match change.action {
                Action::NoOp => {}
                Action::Refresh => {
                    let node = self.push(single(OperationKind::Refresh));
                    self.refresh_of.insert(address, node);
                }
                Action::Delete => {
                    let node = self.push(single(OperationKind::Delete));
                    self.delete_of.insert(address, node);
                }
                Action::Create | Action::Update => {
                    let node = self.push(single(OperationKind::Apply));
                    self.apply_of.insert(address, node);
                }
                Action::Replace => {
                    let delete = self.push(RawOperation {
                        operation: ApplyOperation {
                            change,
                            steps: &change.steps[..1],
                            expected: stored,
                            kind: OperationKind::ReplaceDelete,
                        },
                        change: index,
                        counterpart: None,
                    });
                    let create = self.push(RawOperation {
                        operation: ApplyOperation {
                            change,
                            steps: &change.steps[1..],
                            expected: RecordVersion::Absent,
                            kind: OperationKind::ReplaceCreate,
                        },
                        change: index,
                        counterpart: Some(delete),
                    });
                    self.operations[delete].counterpart = Some(create);
                    self.delete_of.insert(address.clone(), delete);
                    self.apply_of.insert(address, create);
                }
            }
        }
    }

    fn require(&mut self, later: usize, earlier: usize) {
        if later != earlier {
            self.waits_for[later].insert(earlier);
            self.waited_on_by[earlier].insert(later);
        }
    }

    /// Whether `earlier` already has to run before `later`.
    fn precedes(&self, earlier: usize, later: usize) -> bool {
        let mut seen = BTreeSet::new();
        let mut pending = vec![earlier];
        while let Some(node) = pending.pop() {
            if node == later {
                return true;
            }
            if seen.insert(node) {
                pending.extend(self.waited_on_by[node].iter().copied());
            }
        }
        false
    }

    /// Add an edge unless it would make the graph cyclic.
    fn prefer(&mut self, later: usize, earlier: usize) {
        if later != earlier && !self.precedes(later, earlier) {
            self.require(later, earlier);
        }
    }

    /// For every address, the delete nodes of the resources whose stored
    /// record depends on it.
    fn deleted_dependents(&self) -> BTreeMap<String, Vec<usize>> {
        let mut dependents: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for (address, &node) in &self.delete_of {
            let index = self.operations[node].change;
            for dependency in &self.dependencies.stored[index] {
                if dependency != address {
                    dependents.entry(dependency.clone()).or_default().push(node);
                }
            }
        }
        dependents
    }

    fn add_required_edges(&mut self) {
        let dependents = self.deleted_dependents();
        let none = Vec::new();
        for node in 0..self.operations.len() {
            let index = self.operations[node].change;
            let kind = self.operations[node].operation.kind;
            let address = self.changes[index].address.to_string();
            let own_dependents = dependents.get(&address).unwrap_or(&none);
            match kind {
                OperationKind::ReplaceCreate => {
                    if let Some(delete) = self.operations[node].counterpart {
                        self.require(node, delete);
                    }
                    self.require_prerequisites(node, index);
                    // Terraform's `creators` edge.
                    for &dependent in own_dependents {
                        self.require(node, dependent);
                    }
                }
                OperationKind::Apply => {
                    self.require_prerequisites(node, index);
                    for &dependent in own_dependents {
                        self.require(node, dependent);
                    }
                }
                OperationKind::Delete | OperationKind::ReplaceDelete => {
                    // The dependents of a resource are deleted before it.
                    for &dependent in own_dependents {
                        self.require(node, dependent);
                    }
                }
                OperationKind::Refresh => self.require_prerequisites(node, index),
            }
        }
    }

    /// The create or update of a resource waits for the create or update of
    /// everything it is configured to depend on.
    fn require_prerequisites(&mut self, node: usize, index: usize) {
        let prerequisites: Vec<usize> = self.dependencies.configured[index]
            .iter()
            .filter_map(|prerequisite| self.apply_of.get(prerequisite).copied())
            .collect();
        for earlier in prerequisites {
            self.require(node, earlier);
        }
    }

    fn add_preferred_edges(&mut self) {
        // Detachment first: it keeps a delete from failing, where the safety
        // edge only limits the damage of a failure.
        for index in 0..self.changes.len() {
            let change = &self.changes[index];
            let address = change.address.to_string();
            let node = match change.action {
                Action::Update => self.apply_of.get(&address),
                Action::Refresh => self.refresh_of.get(&address),
                _ => None,
            };
            let Some(&node) = node else {
                continue;
            };
            let detached: Vec<usize> = self.dependencies.stored[index]
                .iter()
                .filter(|dependency| !self.dependencies.configured[index].contains(*dependency))
                .filter_map(|dependency| self.delete_of.get(dependency).copied())
                .collect();
            for delete in detached {
                self.prefer(delete, node);
            }
        }
        for index in 0..self.changes.len() {
            let change = &self.changes[index];
            if change.action != Action::Replace {
                continue;
            }
            let Some(&delete) = self.delete_of.get(&change.address.to_string()) else {
                continue;
            };
            let prerequisites: Vec<usize> = self.dependencies.configured[index]
                .iter()
                .filter_map(|prerequisite| self.apply_of.get(prerequisite).copied())
                .collect();
            for earlier in prerequisites {
                self.prefer(delete, earlier);
            }
        }
    }

    /// Rank every declared change in dependency order, ties broken by name.
    fn ranks(&self) -> Result<BTreeMap<String, usize>> {
        let declared: Vec<usize> = (0..self.changes.len())
            .filter(|index| self.changes[*index].action != Action::Delete)
            .collect();
        let name_of: BTreeMap<String, &str> = declared
            .iter()
            .map(|index| {
                let address = &self.changes[*index].address;
                (address.to_string(), address.name.as_str())
            })
            .collect();
        let graph: BTreeMap<String, Vec<String>> = declared
            .iter()
            .map(|index| {
                let dependencies = self.dependencies.configured[*index]
                    .iter()
                    .filter_map(|address| name_of.get(address))
                    .map(|name| (*name).to_string())
                    .collect();
                (self.changes[*index].address.name.clone(), dependencies)
            })
            .collect();
        let order = topological_order(&graph)?;
        let by_name: BTreeMap<&str, usize> = order
            .iter()
            .enumerate()
            .map(|(rank, name)| (name.as_str(), rank))
            .collect();
        Ok(declared
            .iter()
            .filter_map(|index| {
                let address = &self.changes[*index].address;
                by_name
                    .get(address.name.as_str())
                    .map(|rank| (address.to_string(), *rank))
            })
            .collect())
    }

    fn into_schedule(self) -> Result<Schedule<'plan>> {
        let ranks = self.ranks()?;
        let delete_ranks: BTreeMap<&str, usize> = self
            .delete_of
            .keys()
            .enumerate()
            .map(|(rank, address)| (address.as_str(), rank))
            .collect();
        let priority = |node: usize| -> (u8, usize, usize) {
            let raw = &self.operations[node];
            let address = raw.operation.change.address.to_string();
            let kind = raw.operation.kind;
            let rank = match kind {
                OperationKind::Delete | OperationKind::ReplaceDelete => {
                    delete_ranks.get(address.as_str()).copied()
                }
                _ => ranks.get(&address).copied(),
            };
            (kind.priority(), rank.unwrap_or(usize::MAX), node)
        };

        let mut remaining: Vec<usize> = self.waits_for.iter().map(BTreeSet::len).collect();
        let mut ready: BTreeSet<(u8, usize, usize)> = (0..self.operations.len())
            .filter(|node| remaining[*node] == 0)
            .map(priority)
            .collect();
        let mut order = Vec::with_capacity(self.operations.len());
        let mut follow: Option<usize> = None;
        loop {
            let picked = follow
                .take()
                .and_then(|node| ready.take(&priority(node)))
                .or_else(|| ready.pop_first());
            let Some((_, _, next)) = picked else {
                break;
            };
            order.push(next);
            if self.operations[next].operation.kind == OperationKind::ReplaceDelete {
                follow = self.operations[next].counterpart;
            }
            for &successor in &self.waited_on_by[next] {
                remaining[successor] -= 1;
                if remaining[successor] == 0 {
                    ready.insert(priority(successor));
                }
            }
        }
        if order.len() != self.operations.len() {
            return Err(self.cycle_error(&remaining));
        }
        Ok(self.finish(&order))
    }

    fn label(&self, node: usize) -> String {
        let operation = &self.operations[node].operation;
        format!("{} {}", operation.kind.label(), operation.change.address)
    }

    /// Name a cycle among the nodes that never became ready.
    fn cycle_error(&self, remaining: &[usize]) -> InfrastructureError {
        let stuck: BTreeSet<usize> = (0..self.operations.len())
            .filter(|node| remaining[*node] > 0)
            .collect();
        let mut path: Vec<usize> = Vec::new();
        let mut current = stuck.iter().next().copied();
        while let Some(node) = current {
            if let Some(start) = path.iter().position(|seen| *seen == node) {
                let cycle: Vec<String> = path[start..].iter().map(|n| self.label(*n)).collect();
                return InfrastructureError::configuration(format!(
                    "the planned operations depend on each other in a cycle, so no order can \
                     apply them safely: {} -> {}; change the configuration so the dependencies \
                     can be applied one at a time",
                    cycle.join(" -> "),
                    cycle.first().cloned().unwrap_or_default()
                ));
            }
            path.push(node);
            current = self.waits_for[node]
                .iter()
                .copied()
                .find(|earlier| stuck.contains(earlier));
        }
        InfrastructureError::configuration("the planned operations cannot be ordered")
    }

    fn finish(self, order: &[usize]) -> Schedule<'plan> {
        let mut position = vec![0; order.len()];
        for (new, old) in order.iter().enumerate() {
            position[*old] = new;
        }
        let mut first_position: BTreeMap<usize, usize> = BTreeMap::new();
        for (new, old) in order.iter().enumerate() {
            first_position
                .entry(self.operations[*old].change)
                .or_insert(new);
        }
        let mut change_order: Vec<usize> = (0..self.changes.len()).collect();
        change_order.sort_by_key(|index| {
            (
                first_position.get(index).copied().unwrap_or(usize::MAX),
                self.changes[*index].address.to_string(),
            )
        });
        let Self {
            operations,
            waited_on_by,
            ..
        } = self;
        let mut slots: Vec<Option<RawOperation<'plan>>> =
            operations.into_iter().map(Some).collect();
        let operations = order
            .iter()
            .filter_map(|old| {
                slots[*old].take().map(|raw| ScheduledOperation {
                    operation: raw.operation,
                    successors: waited_on_by[*old]
                        .iter()
                        .map(|node| position[*node])
                        .collect(),
                    counterpart: raw.counterpart.map(|node| position[node]),
                })
            })
            .collect();
        Schedule {
            operations,
            change_order,
        }
    }
}

/// Where an operation stands during an apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OperationState {
    Pending,
    Finished,
    Failed,
    Skipped,
}

/// Tracks an apply through its schedule: what finished, what failed, what
/// was skipped because of a failure, and which replacements lost their old
/// object without gaining a new one.
pub(super) struct ScheduleRun<'schedule, 'plan> {
    schedule: &'schedule Schedule<'plan>,
    states: Vec<OperationState>,
    failures: Vec<ChangeFailure>,
    skipped: Vec<ResourceAddress>,
    reported: BTreeSet<ResourceAddress>,
    deleted_not_recreated: BTreeSet<ResourceAddress>,
}

impl<'schedule, 'plan> ScheduleRun<'schedule, 'plan> {
    pub(super) fn new(schedule: &'schedule Schedule<'plan>) -> Self {
        Self {
            schedule,
            states: vec![OperationState::Pending; schedule.operations.len()],
            failures: Vec::new(),
            skipped: Vec::new(),
            reported: BTreeSet::new(),
            deleted_not_recreated: BTreeSet::new(),
        }
    }

    /// The schedule being run.
    pub(super) const fn schedule(&self) -> &'schedule Schedule<'plan> {
        self.schedule
    }

    pub(super) fn is_skipped(&self, node: usize) -> bool {
        self.states[node] == OperationState::Skipped
    }

    /// The operation finished and was recorded.
    pub(super) fn finished(&mut self, node: usize) {
        self.states[node] = OperationState::Finished;
        let operation = &self.schedule.operations[node].operation;
        match operation.kind {
            OperationKind::ReplaceDelete => {
                self.deleted_not_recreated
                    .insert(operation.change.address.clone());
            }
            OperationKind::ReplaceCreate => {
                self.deleted_not_recreated.remove(&operation.change.address);
            }
            _ => {}
        }
    }

    /// The operation failed: nothing that depends on it runs, and a
    /// replacement that has not been started is not started at all if its
    /// create can no longer follow.
    pub(super) fn failed(
        &mut self,
        node: usize,
        error: InfrastructureError,
        on_event: &mut (dyn FnMut(ApplyEvent) + Send),
    ) {
        let schedule = self.schedule;
        self.states[node] = OperationState::Failed;
        let operation = &schedule.operations[node].operation;
        self.reported.insert(operation.change.address.clone());
        on_event(ApplyEvent::Failed {
            address: operation.change.address.clone(),
            action: operation.change.action,
        });
        self.failures.push(ChangeFailure {
            address: operation.change.address.clone(),
            error,
        });
        let mut pending = vec![node];
        while let Some(current) = pending.pop() {
            let mut skipped: Vec<usize> = self.schedule.operations[current]
                .successors
                .iter()
                .copied()
                .filter(|successor| self.states[*successor] == OperationState::Pending)
                .collect();
            // A create that can no longer run takes a delete that has not
            // started with it: the old object is only destroyed when the new
            // one can follow.
            let scheduled = &self.schedule.operations[current];
            if scheduled.operation.kind == OperationKind::ReplaceCreate
                && let Some(delete) = scheduled.counterpart
                && self.states[delete] == OperationState::Pending
            {
                skipped.push(delete);
            }
            for next in skipped {
                if self.states[next] == OperationState::Pending {
                    self.skip(next, on_event);
                    pending.push(next);
                }
            }
        }
    }

    fn skip(&mut self, node: usize, on_event: &mut (dyn FnMut(ApplyEvent) + Send)) {
        self.states[node] = OperationState::Skipped;
        let change = self.schedule.operations[node].operation.change;
        let lost_old_object = self.deleted_not_recreated.contains(&change.address);
        if !lost_old_object && self.reported.insert(change.address.clone()) {
            self.skipped.push(change.address.clone());
            on_event(ApplyEvent::Skipped {
                address: change.address.clone(),
                action: change.action,
            });
        }
    }

    /// Replacements whose old object was deleted but whose new object was
    /// not created.
    pub(super) fn deleted_not_recreated(&self) -> Vec<ResourceAddress> {
        self.deleted_not_recreated.iter().cloned().collect()
    }

    /// Report every deleted-not-recreated replacement.
    pub(super) fn report_uncreated(&self, on_event: &mut (dyn FnMut(ApplyEvent) + Send)) {
        for address in &self.deleted_not_recreated {
            on_event(ApplyEvent::DeletedNotRecreated {
                address: address.clone(),
            });
        }
    }

    /// The run ends early with an error of its own (an interrupt, a state
    /// store failure): report what it leaves behind, including failures of
    /// earlier operations that the error would otherwise hide.
    pub(super) fn abandon(&self, on_event: &mut (dyn FnMut(ApplyEvent) + Send)) {
        for failure in &self.failures {
            on_event(ApplyEvent::Warning(format!(
                "{} failed earlier in this run: {}",
                failure.address, failure.error
            )));
        }
        self.report_uncreated(on_event);
    }

    pub(super) fn has_failures(&self) -> bool {
        !self.failures.is_empty()
    }

    pub(super) fn take_failures(&mut self) -> Vec<ChangeFailure> {
        std::mem::take(&mut self.failures)
    }

    pub(super) fn take_skipped(&mut self) -> Vec<ResourceAddress> {
        std::mem::take(&mut self.skipped)
    }
}
