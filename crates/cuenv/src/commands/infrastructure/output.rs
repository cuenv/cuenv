//! Text and JSON output for `cuenv infrastructure`.
//!
//! In JSON mode a command prints exactly one envelope on standard output,
//! as its last act; progress, warnings and prompts go to standard error
//! (`main` renders events there for this command in JSON mode). Nothing
//! printed here ever contains attribute values.

use cuenv_events::{emit_stderr, emit_stdout};
use cuenv_infrastructure::{
    Action, LockInformation, ManagedResource, Plan, PlanMode, PlanSummary, TenantKey,
};
use serde_json::{Value, json};

use crate::cli::OutputFormat;

/// The JSON name of a planned action. An explicit mapping, so renaming a
/// Rust variant never changes the JSON contract.
#[must_use]
pub(super) const fn action_name(action: Action) -> &'static str {
    match action {
        Action::NoOp => "no-op",
        Action::Create => "create",
        Action::Update => "update",
        Action::Replace => "replace",
        Action::Delete => "delete",
    }
}

/// The JSON name of what a converging command does.
#[must_use]
pub(super) const fn operation_name(mode: PlanMode) -> &'static str {
    match mode {
        PlanMode::Apply => "apply",
        PlanMode::Destroy => "destroy",
    }
}

/// Where the command's output goes.
#[derive(Debug, Clone, Copy)]
pub(super) struct Output {
    format: OutputFormat,
}

impl Output {
    /// Output in the given format.
    #[must_use]
    pub(super) const fn new(format: OutputFormat) -> Self {
        Self { format }
    }

    /// Whether the command's result is one JSON envelope.
    #[must_use]
    pub(super) const fn is_json(self) -> bool {
        self.format.is_json()
    }

    /// A line of the human-readable result (text mode only).
    pub(super) fn text(self, line: impl std::fmt::Display) {
        if !self.is_json() {
            emit_stdout!(line.to_string());
        }
    }

    /// Progress while applying: the result in text mode, a diagnostic on
    /// standard error in JSON mode.
    pub(super) fn progress(self, line: impl std::fmt::Display) {
        if self.is_json() {
            emit_stderr!(line.to_string());
        } else {
            emit_stdout!(line.to_string());
        }
    }

    /// A plan: rendered in text mode, the result envelope in JSON mode.
    pub(super) fn plan(self, plan: &Plan) {
        warnings(plan);
        if self.is_json() {
            print_envelope(&plan_json(plan));
        } else {
            render_plan_text(plan);
        }
    }

    /// A plan shown before confirming or applying (text mode only; JSON mode
    /// reports the applied plan once, at the end).
    pub(super) fn preview(self, plan: &Plan) {
        warnings(plan);
        if !self.is_json() {
            render_plan_text(plan);
        }
    }

    /// A finished apply or destroy.
    pub(super) fn converged(self, result: &Converged<'_>) {
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
            print_envelope(&payload);
            return;
        }
        let Some(applied) = result.applied else {
            return;
        };
        match result.mode {
            PlanMode::Apply => emit_stdout!(format!(
                "Apply complete: {} created, {} updated, {} replaced, {} deleted.",
                applied.create, applied.update, applied.replace, applied.delete
            )),
            PlanMode::Destroy => {
                emit_stdout!(format!("Destroy complete: {} destroyed.", applied.delete));
            }
        }
    }

    /// Managed resources recorded for a tenant.
    pub(super) fn state(self, tenant: &TenantKey, resources: &[ManagedResource]) {
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
            print_envelope(&json!({"tenant": tenant.to_string(), "resources": rows}));
            return;
        }
        if resources.is_empty() {
            emit_stdout!(format!("No managed resources for {tenant}"));
            return;
        }
        emit_stdout!(format!("{:<40} {:<12} {}", "ADDRESS", "PROVIDER", "SOURCE"));
        for resource in resources {
            let marker = if resource.tainted { " (tainted)" } else { "" };
            emit_stdout!(format!(
                "{:<40} {:<12} {}{marker}",
                resource.address.to_string(),
                resource.provider,
                resource.provider_source
            ));
        }
    }

    /// The state of a tenant's lock, and whether this run released it.
    pub(super) fn lock(self, report: &LockReport<'_>) {
        if self.is_json() {
            print_envelope(&json!({
                "tenant": report.tenant.to_string(),
                "lock": report.lock.map(|lock| json!({
                    "lockIdentifier": lock.lock_identifier,
                    "holder": lock.holder,
                    "acquiredAt": lock.acquired_at,
                })),
                "released": matches!(report.outcome, LockOutcome::Released),
            }));
            return;
        }
        let tenant = report.tenant;
        match (report.lock, report.outcome) {
            (None, _) => emit_stdout!(format!("{tenant} is not locked")),
            (Some(lock), LockOutcome::Shown) => emit_stdout!(format!(
                "{tenant} is locked by '{}' since {} (lock {}).\n\
                 Confirm that run is gone, then release it with \
                 `cuenv infrastructure unlock {}`.",
                lock.holder, lock.acquired_at, lock.lock_identifier, lock.lock_identifier
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
}

fn warnings(plan: &Plan) {
    for warning in &plan.warnings {
        emit_stderr!(format!("warning: {warning}"));
    }
}

fn render_plan_text(plan: &Plan) {
    emit_stdout!(format!("cuenv infrastructure: {}", plan.tenant));
    if plan.has_changes() {
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
                "action": action_name(change.action),
                "requiresReplace": change.requires_replace,
            })
        })
        .collect();
    json!({
        "tenant": plan.tenant.to_string(),
        "changes": changes,
        "summary": summary_json(&plan.summary()),
    })
}

/// Print a JSON result in cuenv's standard success envelope.
fn print_envelope(payload: &Value) {
    let envelope = crate::cli::OkEnvelope::new(payload);
    match serde_json::to_string(&envelope) {
        Ok(json) => cuenv_events::println_redacted(&json),
        Err(error) => emit_stderr!(format!("error: could not serialize JSON output: {error}")),
    }
}
