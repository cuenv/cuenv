//! Text and JSON output for `cuenv infrastructure`.
//!
//! In JSON mode a command prints exactly one document on standard output:
//! the result envelope, held back until the command is over (its lock
//! released), or the error envelope `main` renders on failure, or, after a
//! second interrupt, the error envelope the forced exit writes. The
//! [`ResultGate`] makes those three mutually exclusive. Progress, warnings
//! and prompts go to standard error (`main` renders events there for this
//! command in JSON mode). Nothing printed here ever contains attribute
//! values, and text read from the state store is stripped of control
//! characters.

use std::io::Write;
use std::sync::{Arc, Mutex, PoisonError};

use cuenv_events::{emit_stderr, emit_stdout};
use cuenv_infrastructure::{
    LockInformation, ManagedResource, Plan, PlanMode, PlanSummary, ResourceAddress, TenantKey,
    TenantOwner, strip_control_characters,
};
use serde_json::{Value, json};

use super::tenant_label;
use crate::cli::OutputFormat;

/// The project part of a tenant, without its environment: the JSON `tenant`
/// field. The environment is reported in its own `environment` field, so a
/// consumer never has to split a name apart.
fn tenant_identity(tenant: &TenantKey) -> String {
    format!("{}#{}", tenant.module_path(), tenant.project())
}

/// The JSON name of what a converging command does.
#[must_use]
pub(super) const fn operation_name(mode: PlanMode) -> &'static str {
    match mode {
        PlanMode::Apply => "apply",
        PlanMode::Destroy => "destroy",
    }
}

/// Who writes the one document on standard output in JSON mode.
#[derive(Debug, Default)]
pub(super) struct ResultGate {
    state: Mutex<GateState>,
}

#[derive(Debug, Default)]
enum GateState {
    /// The command is running and has produced no result yet.
    #[default]
    Open,
    /// The command produced this result, printed when it finishes.
    Pending(Value),
    /// The command finished; it (or `main`, for an error) prints the result.
    Finished,
    /// A forced exit writes the only document and ends the process.
    Exiting,
}

/// How the command ends, as the gate decides.
#[derive(Debug)]
pub(super) enum Finish {
    /// Print this result (or nothing) and return normally.
    Report(Option<Value>),
    /// A forced exit is writing the only document and ending the process;
    /// print nothing and never return.
    Exiting,
}

impl ResultGate {
    fn lock(&self) -> std::sync::MutexGuard<'_, GateState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Keep the command's result for when it finishes.
    fn set(&self, payload: Value) {
        let mut state = self.lock();
        if matches!(*state, GateState::Open | GateState::Pending(_)) {
            *state = GateState::Pending(payload);
        }
    }

    /// The command is over: take its result, unless a forced exit already
    /// claimed standard output.
    pub(super) fn finish(&self) -> Finish {
        let mut state = self.lock();
        match std::mem::replace(&mut *state, GateState::Finished) {
            GateState::Exiting => {
                *state = GateState::Exiting;
                Finish::Exiting
            }
            GateState::Pending(payload) => Finish::Report(Some(payload)),
            GateState::Open | GateState::Finished => Finish::Report(None),
        }
    }

    /// A forced exit wants standard output. Returns `false` when the command
    /// already finished and reports its own outcome.
    pub(super) fn claim_for_exit(&self) -> bool {
        let mut state = self.lock();
        match *state {
            GateState::Open | GateState::Pending(_) => {
                *state = GateState::Exiting;
                true
            }
            GateState::Exiting => true,
            GateState::Finished => false,
        }
    }
}

/// Where the command's output goes.
#[derive(Debug, Clone)]
pub(super) struct Output {
    format: OutputFormat,
    gate: Arc<ResultGate>,
}

impl Output {
    /// Output in the given format.
    #[must_use]
    pub(super) fn new(format: OutputFormat) -> Self {
        Self {
            format,
            gate: Arc::default(),
        }
    }

    /// The format the command reports in.
    #[must_use]
    pub(super) const fn format(&self) -> OutputFormat {
        self.format
    }

    /// Whether the command's result is one JSON envelope.
    #[must_use]
    pub(super) const fn is_json(&self) -> bool {
        self.format.is_json()
    }

    /// The gate over standard output, shared with the forced exit.
    #[must_use]
    pub(super) fn gate(&self) -> Arc<ResultGate> {
        Arc::clone(&self.gate)
    }

    /// The command is over: take its JSON result, if it has one, for the
    /// caller to print with [`print_envelope`] when the command succeeded.
    /// Returns [`Finish::Exiting`] when a forced exit owns standard output.
    pub(super) fn finish(&self) -> Finish {
        self.gate.finish()
    }

    fn envelope(&self, payload: Value) {
        self.gate.set(payload);
    }

    /// Progress while applying: the result in text mode, a diagnostic on
    /// standard error in JSON mode.
    pub(super) fn progress(&self, line: impl std::fmt::Display) {
        if self.is_json() {
            emit_stderr!(line.to_string());
        } else {
            emit_stdout!(line.to_string());
        }
    }

    /// A plan: rendered in text mode, the result envelope in JSON mode.
    pub(super) fn plan(&self, plan: &Plan) {
        warnings(plan);
        if self.is_json() {
            self.envelope(plan_json(plan));
        } else {
            render_plan_text(plan);
        }
    }

    /// A plan shown before confirming or applying (text mode only; JSON mode
    /// reports the applied plan once, at the end).
    pub(super) fn preview(&self, plan: &Plan) {
        warnings(plan);
        if !self.is_json() {
            render_plan_text(plan);
        }
    }

    /// A finished apply or destroy.
    pub(super) fn converged(&self, result: &Converged<'_>) {
        if self.is_json() {
            let mut payload = plan_json(result.plan);
            if let Value::Object(fields) = &mut payload {
                fields.insert("operation".to_string(), json!(operation_name(result.mode)));
                fields.insert(
                    "applied".to_string(),
                    result
                        .applied
                        .map_or(Value::Null, |applied| summary_json(&applied)),
                );
            }
            self.envelope(payload);
            return;
        }
        let Some(applied) = result.applied else {
            return;
        };
        match result.mode {
            PlanMode::Apply => emit_stdout!(format!(
                "Apply complete: {} created, {} updated, {} replaced, {} deleted, {} refreshed.",
                applied.create, applied.update, applied.replace, applied.delete, applied.refresh
            )),
            PlanMode::Destroy => {
                emit_stdout!(format!("Destroy complete: {} destroyed.", applied.delete));
            }
        }
    }

    /// A resource forgotten by `state remove`.
    pub(super) fn removed(&self, tenant: &TenantKey, address: &ResourceAddress) {
        if self.is_json() {
            self.envelope(json!({
                "tenant": tenant_identity(tenant),
                "environment": tenant.environment(),
                "removed": address.to_string(),
            }));
        } else {
            emit_stdout!(format!(
                "Removed {address} from the state of {}. The real object was not touched.",
                tenant_label(tenant)
            ));
        }
    }

    /// Records written by `state recover`.
    pub(super) fn recovered(&self, tenant: &TenantKey, addresses: &[ResourceAddress]) {
        if self.is_json() {
            let recovered: Vec<String> = addresses.iter().map(ToString::to_string).collect();
            self.envelope(json!({
                "tenant": tenant_identity(tenant),
                "environment": tenant.environment(),
                "recovered": recovered,
            }));
            return;
        }
        if addresses.is_empty() {
            emit_stdout!(format!(
                "No unrecorded changes for {}.",
                tenant_label(tenant)
            ));
            return;
        }
        for address in addresses {
            emit_stdout!(format!("Recorded {address}"));
        }
        emit_stdout!(format!(
            "Recovered {} unrecorded change(s) for {}.",
            addresses.len(),
            tenant_label(tenant)
        ));
    }

    /// An ownership transfer by `state adopt`.
    pub(super) fn adopted(&self, adoption: &Adoption<'_>) {
        let owner_json = |owner: &TenantOwner| {
            json!({
                "instance": strip_control_characters(owner.instance.as_str()),
                "claimedAt": strip_control_characters(&owner.claimed_at),
            })
        };
        if self.is_json() {
            self.envelope(json!({
                "tenant": tenant_identity(adoption.tenant),
                "environment": adoption.tenant.environment(),
                "previousOwner": adoption.previous.map(owner_json),
                "owner": owner_json(adoption.owner),
            }));
            return;
        }
        let owner = strip_control_characters(adoption.owner.instance.as_str());
        match adoption.previous {
            None => emit_stdout!(format!(
                "{} had no owner; {owner} now owns its state.",
                tenant_label(adoption.tenant)
            )),
            Some(previous) if previous.instance == adoption.owner.instance => emit_stdout!(
                format!(
                    "{owner} already owns the state of {}.",
                    tenant_label(adoption.tenant)
                )
            ),
            Some(previous) => emit_stdout!(format!(
                "Ownership of {} moved from {} (since {}) to {owner}.",
                tenant_label(adoption.tenant),
                strip_control_characters(previous.instance.as_str()),
                strip_control_characters(&previous.claimed_at)
            )),
        }
    }

    /// Managed resources recorded for a tenant.
    pub(super) fn state(&self, tenant: &TenantKey, resources: &[ManagedResource]) {
        if self.is_json() {
            let rows: Vec<Value> = resources
                .iter()
                .map(|resource| {
                    json!({
                        "address": resource.address.to_string(),
                        "provider": resource.provider,
                        "providerSource": resource.provider_source,
                        "schemaVersion": resource.schema_version,
                        "tainted": resource.tainted,
                    })
                })
                .collect();
            self.envelope(json!({
                "tenant": tenant_identity(tenant),
                "environment": tenant.environment(),
                "resources": rows,
            }));
            return;
        }
        if resources.is_empty() {
            emit_stdout!(format!("No managed resources for {}", tenant_label(tenant)));
            return;
        }
        emit_stdout!(format!("{:<40} {:<12} {}", "ADDRESS", "PROVIDER", "SOURCE"));
        for resource in resources {
            let marker = if resource.tainted { " (tainted)" } else { "" };
            emit_stdout!(format!(
                "{:<40} {:<12} {}{marker}",
                strip_control_characters(&resource.address.to_string()),
                strip_control_characters(&resource.provider),
                strip_control_characters(&resource.provider_source)
            ));
        }
    }

    /// The state of a tenant's lock, and whether this run released it.
    pub(super) fn lock(&self, report: &LockReport<'_>) {
        let lock = report.lock.map(|lock| LockInformation {
            lock_identifier: strip_control_characters(&lock.lock_identifier),
            holder: strip_control_characters(&lock.holder),
            acquired_at: strip_control_characters(&lock.acquired_at),
        });
        if self.is_json() {
            self.envelope(json!({
                "tenant": tenant_identity(report.tenant),
                "environment": report.tenant.environment(),
                "lock": lock.as_ref().map(|lock| json!({
                    "lockIdentifier": lock.lock_identifier,
                    "holder": lock.holder,
                    "acquiredAt": lock.acquired_at,
                })),
                "released": matches!(report.outcome, LockOutcome::Released),
            }));
            return;
        }
        let tenant = tenant_label(report.tenant);
        match (lock, report.outcome) {
            (None, _) => emit_stdout!(format!("{tenant} is not locked")),
            (Some(lock), LockOutcome::Shown) => emit_stdout!(format!(
                "{tenant} is locked by '{}' since {} (lock {}).\n\
                 Confirm that run is gone, then release it with `{}`.",
                lock.holder,
                lock.acquired_at,
                lock.lock_identifier,
                report
                    .unlock_command
                    .as_deref()
                    .unwrap_or("cuenv infrastructure unlock <lock identifier>")
            )),
            (Some(lock), LockOutcome::Released) => emit_stdout!(format!(
                "Released lock {} held by '{}' on {tenant}",
                lock.lock_identifier, lock.holder
            )),
        }
    }
}

/// What a converging command did.
#[derive(Debug)]
pub(super) struct Converged<'result> {
    /// Apply or destroy.
    pub(super) mode: PlanMode,
    /// The plan that was applied.
    pub(super) plan: &'result Plan,
    /// Counts of applied changes; `None` when there was nothing to apply.
    pub(super) applied: Option<PlanSummary>,
}

/// An ownership transfer.
#[derive(Debug)]
pub(super) struct Adoption<'adoption> {
    /// Tenant whose owner changed.
    pub(super) tenant: &'adoption TenantKey,
    /// The owner before, if any.
    pub(super) previous: Option<&'adoption TenantOwner>,
    /// The owner now.
    pub(super) owner: &'adoption TenantOwner,
}

/// Whether `unlock` only showed the lock or released it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LockOutcome {
    /// The lock (or its absence) was shown.
    Shown,
    /// The lock was released.
    Released,
}

/// A tenant's lock as `unlock` reports it.
#[derive(Debug)]
pub(super) struct LockReport<'report> {
    /// Tenant.
    pub(super) tenant: &'report TenantKey,
    /// The lock, when one is held.
    pub(super) lock: Option<&'report LockInformation>,
    /// What `unlock` did.
    pub(super) outcome: LockOutcome,
    /// The command that releases the lock shown, naming this run's
    /// environment and project.
    pub(super) unlock_command: Option<String>,
}

fn warnings(plan: &Plan) {
    for warning in &plan.warnings {
        emit_stderr!(format!("warning: {warning}"));
    }
}

fn render_plan_text(plan: &Plan) {
    emit_stdout!(format!("cuenv infrastructure: {}", tenant_label(&plan.tenant)));
    if plan.has_work() {
        emit_stdout!(cuenv_infrastructure::render_plan(plan));
    } else {
        emit_stdout!("No changes. Infrastructure matches the configuration.");
    }
}

fn summary_json(summary: &PlanSummary) -> Value {
    json!({
        "create": summary.create,
        "update": summary.update,
        "replace": summary.replace,
        "delete": summary.delete,
        "refresh": summary.refresh,
        "unchanged": summary.unchanged,
    })
}

/// A plan as JSON: addresses, actions and counts, never attribute values.
#[must_use]
pub(super) fn plan_json(plan: &Plan) -> Value {
    let changes: Vec<Value> = plan
        .changes
        .iter()
        .map(|change| {
            json!({
                "address": change.address.to_string(),
                "action": change.action.name(),
                "requiresReplace": change.requires_replace,
            })
        })
        .collect();
    json!({
        "tenant": tenant_identity(&plan.tenant),
        "environment": plan.tenant.environment(),
        "changes": changes,
        "summary": summary_json(&plan.summary()),
    })
}

/// Print a JSON result in cuenv's standard success envelope.
pub(super) fn print_envelope(payload: &Value) {
    let envelope = crate::cli::OkEnvelope::new(payload);
    // Redact the strings inside the document, never its serialized text,
    // where a secret that JSON escapes would not be found.
    let rendered = serde_json::to_value(&envelope).and_then(|mut document| {
        cuenv_events::redact_json_value(&mut document);
        serde_json::to_string(&document)
    });
    match rendered {
        Ok(json) => {
            let mut standard_output = std::io::stdout().lock();
            if let Err(error) = writeln!(standard_output, "{json}") {
                tracing::debug!(%error, "failed to write the JSON result");
            }
        }
        Err(error) => emit_stderr!(format!("error: could not serialize JSON output: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_result_waits_for_the_end_of_the_command() {
        let gate = ResultGate::default();
        gate.set(json!({"first": true}));
        gate.set(json!({"second": true}));
        assert!(matches!(
            gate.finish(),
            Finish::Report(Some(payload)) if payload == json!({"second": true})
        ));
        // Once the command finished, a forced exit leaves the output alone.
        assert!(!gate.claim_for_exit());
    }

    #[test]
    fn a_forced_exit_owns_standard_output() {
        let gate = ResultGate::default();
        gate.set(json!({"applied": true}));
        assert!(gate.claim_for_exit());
        gate.set(json!({"late": true}));
        assert!(matches!(gate.finish(), Finish::Exiting));
        assert!(matches!(gate.finish(), Finish::Exiting));
    }
}
