//! Implementation of `cuenv infrastructure` (short form `cuenv i`).
//!
//! Evaluates the project's `infrastructure` block and hands it to
//! `cuenv-infrastructure`, which drives Terraform provider plugins over gRPC
//! and stores each managed resource in the configured state database (Turso
//! or Cloudflare D1). State
//! is keyed by the CUE module path and the project name, and owned by one
//! CUE instance (directory and package).
//!
//! `apply` and `destroy` follow Terraform: take the lock, plan, show the
//! plan and (unless `--yes`) ask for confirmation while holding the lock,
//! then apply exactly that plan. Every write happens under [`under_lock`](lock::under_lock),
//! which also creates or upgrades the state tables first, so commands that
//! only read never do. The command owns interrupt handling for its whole
//! run (see [`interrupts`]).

mod action;
mod apply;
mod confirm;
mod dispatch;
mod evaluation;
mod failure;
mod guard;
mod holder;
mod interrupts;
mod invocation;
pub(crate) mod locator;
mod lock;
mod migrate;
mod object_state;
mod output;
mod output_selectors;
mod provider;
mod provider_environment;
mod resolve;
mod run;
mod state_actions;
pub(crate) mod types;

use self::failure::failure;
use self::guard::tenant_label;
use self::interrupts::Interrupts;
use self::invocation::Invocation;
use self::output::{Finish, Output};
use self::run::run;
use crate::cli::CliError;

pub use self::action::{
    ConfirmationPolicy, FenceRelease, Identity, InfrastructureAction, InfrastructureOptions,
    LockListing, MigrateRequest, MigrationDirection, MigrationScope, RenameSource, SeparateState,
    SourceEnvironment, SourceUpgrade, StateAction, UnlockScope,
};
pub use self::provider::{ProviderAction, execute_provider};

/// Execute `cuenv infrastructure`.
///
/// The command claims interrupt signals before doing anything else and
/// keeps them until it returns. In JSON mode its result envelope is printed
/// last, after the lock is released.
///
/// # Errors
///
/// Returns an error if evaluation fails, the project has no `infrastructure`
/// block, its name is not unique in the module, another instance owns its
/// state, the state store is unreachable or locked, the plan is not
/// confirmed, a provider fails, or the run is interrupted.
#[tracing::instrument(skip_all, fields(path = %options.path, action = options.action.trace_name()))]
pub async fn execute_infrastructure(options: &InfrastructureOptions) -> Result<(), CliError> {
    // Text read from providers (their log lines and error messages) is
    // redacted as it is read, before control characters are stripped.
    cuenv_infrastructure::plugin::install_log_redactor(cuenv_events::redact);
    let output = Output::new(options.output);
    let interrupts = Interrupts::claim(&output, Invocation::of(options))?;
    let result = run(options, &output, &interrupts).await;
    match output.finish() {
        // A forced exit is writing the only report and ending the process;
        // returning would let `main` print a second one.
        Finish::Exiting => return std::future::pending().await,
        Finish::Report(Some(payload)) if result.is_ok() => output::print_envelope(&payload),
        Finish::Report(_) => {}
    }
    // Dropping the claim aborts the signal watcher.
    drop(interrupts);
    result
}

#[cfg(test)]
mod tests;
