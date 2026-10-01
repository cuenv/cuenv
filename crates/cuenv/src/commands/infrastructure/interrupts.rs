//! Interrupt ownership for `cuenv infrastructure`.
//!
//! The command owns SIGINT, SIGTERM, SIGHUP and SIGQUIT for its whole run:
//! `main` does not race it on Ctrl-C (see `InterruptPolicy` in `main.rs`),
//! and the handlers are installed before evaluation, so they are in place
//! long before the state lock is taken. The semantics follow Terraform's:
//!
//! - First signal: [`Cancellation::stop`]. No new resource is started and
//!   every running provider is asked to stop, so the operation in flight
//!   returns early; whatever it returns is recorded, then the lock is
//!   released. Work that changes nothing (CUE evaluation, state reads, the
//!   confirmation prompt) is abandoned at once; planning stops at the next
//!   resource.
//! - Second signal: [`Cancellation::terminate_providers`] kills every
//!   provider process, a record being written gets about two seconds to
//!   finish, the lock (held, or being acquired) is released if that takes at
//!   most about two seconds, the outcome and lock identifier are written
//!   straight to standard error (and, in JSON mode, as the one error
//!   envelope on standard output), and the process exits with code 130.
//!
//! The lock identifier is chosen before the lock is requested and
//! registered here first, so a forced exit during acquisition can still
//! name the lock that may have been taken.

use std::future::Future;
use std::io::Write;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use cuenv_events::emit_stderr;
use cuenv_infrastructure::{Cancellation, StateLock, StateStore, TenantKey};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use super::invocation::Invocation;
use super::output::{Output, ResultGate};
use crate::cli::{CliError, EXIT_INTERRUPTED, InfrastructureFailureKind, LockStatus, OutputFormat};

/// How long a forced exit waits for the lock to be released.
const RELEASE_BOUND: Duration = Duration::from_secs(2);

/// How long a forced exit waits for a record being written.
const RECORDING_BOUND: Duration = Duration::from_secs(2);

/// A lock the command holds or is acquiring, so a forced exit can try to
/// release it.
#[derive(Clone)]
pub(super) struct HeldLock {
    /// Store holding the lock.
    pub(super) store: Arc<dyn StateStore>,
    /// Tenant the lock belongs to.
    pub(super) tenant: TenantKey,
    /// The lock.
    pub(super) lock: StateLock,
}

impl std::fmt::Debug for HeldLock {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HeldLock")
            .field("tenant", &self.tenant)
            .field("lock", &self.lock)
            .finish_non_exhaustive()
    }
}

/// Where the command is with its lock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LockPhase {
    /// The request was sent; it may or may not have committed.
    Acquiring,
    /// Acquired and not yet released.
    Held,
}

/// What the signal watcher shares with the command.
#[derive(Debug, Default)]
struct Shared {
    lock: Mutex<Option<(LockPhase, HeldLock)>>,
}

impl Shared {
    fn lock(&self) -> Option<(LockPhase, HeldLock)> {
        self.lock
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn set(&self, lock: Option<(LockPhase, HeldLock)>) {
        *self.lock.lock().unwrap_or_else(PoisonError::into_inner) = lock;
    }
}

/// Where interrupt signals come from: the process's signal handlers, or a
/// test's own source.
pub(super) trait SignalSource: Send + 'static {
    /// Wait for the next signal. Resolves to `false` when no more can
    /// arrive.
    fn next(&mut self) -> impl Future<Output = bool> + Send;
}

/// The command's claim on interrupt signals. Dropping it aborts the
/// watcher task.
#[derive(Debug)]
pub(super) struct Interrupts {
    cancellation: Cancellation,
    requested: watch::Receiver<bool>,
    shared: Arc<Shared>,
    watcher: JoinHandle<()>,
}

impl Interrupts {
    /// Install the process's signal handlers and start watching them.
    ///
    /// # Errors
    ///
    /// Returns an error when the handlers cannot be installed; the command
    /// refuses to run rather than run without clean interrupt handling.
    pub(super) fn claim(output: &Output, invocation: Invocation) -> Result<Self, CliError> {
        Ok(Self::watch(Signals::install()?, output, invocation))
    }

    /// Start watching `signals`. A forced exit reports in `output`'s format
    /// and claims standard output through its gate; the commands its hints
    /// name carry `invocation`'s environment and project.
    pub(super) fn watch(
        signals: impl SignalSource,
        output: &Output,
        invocation: Invocation,
    ) -> Self {
        let cancellation = Cancellation::default();
        let (sender, requested) = watch::channel(false);
        let shared = Arc::new(Shared::default());
        let watcher = tokio::spawn(watch_signals(Watcher {
            signals,
            sender,
            exit: ForcedExitContext {
                cancellation: cancellation.clone(),
                shared: Arc::clone(&shared),
                format: output.format(),
                invocation,
                gate: output.gate(),
            },
        }));
        Self {
            cancellation,
            requested,
            shared,
            watcher,
        }
    }

    /// The cancellation handed to the engine; every provider it launches is
    /// registered with it.
    #[must_use]
    pub(super) const fn cancellation(&self) -> &Cancellation {
        &self.cancellation
    }

    /// Whether an interrupt was received.
    #[must_use]
    pub(super) fn is_requested(&self) -> bool {
        self.cancellation.is_stop_requested()
    }

    /// Fail with [`interrupted`] when an interrupt was received.
    ///
    /// # Errors
    ///
    /// Returns the interrupted error.
    pub(super) fn check(&self) -> Result<(), CliError> {
        if self.is_requested() {
            Err(interrupted())
        } else {
            Ok(())
        }
    }

    /// Run work that changes nothing until it finishes or an interrupt
    /// arrives, whichever is first. The work is dropped on interrupt.
    ///
    /// # Errors
    ///
    /// Returns the interrupted error when an interrupt arrives first.
    pub(super) async fn until_interrupted<Output>(
        &self,
        work: impl Future<Output = Output>,
    ) -> Result<Output, CliError> {
        let mut requested = self.requested.clone();
        tokio::select! {
            biased;
            () = wait_for_request(&mut requested) => Err(interrupted()),
            output = work => Ok(output),
        }
    }

    /// Record the lock the command is about to request, before the request
    /// is sent.
    pub(super) fn acquiring(&self, lock: HeldLock) {
        self.shared.set(Some((LockPhase::Acquiring, lock)));
    }

    /// Record that the lock is held.
    pub(super) fn hold(&self, lock: HeldLock) {
        self.shared.set(Some((LockPhase::Held, lock)));
    }

    /// Record that the command released (or gave up on) its lock.
    pub(super) fn released(&self) {
        self.shared.set(None);
    }
}

impl Drop for Interrupts {
    fn drop(&mut self) {
        self.watcher.abort();
    }
}

/// The error for work stopped by an interrupt before it changed anything.
#[must_use]
pub(super) fn interrupted() -> CliError {
    CliError::infrastructure(
        "interrupted; no change was applied",
        None,
        InfrastructureFailureKind::Interrupted,
    )
}

async fn wait_for_request(requested: &mut watch::Receiver<bool>) {
    if requested.wait_for(|requested| *requested).await.is_err() {
        // The watcher is gone, so no interrupt can arrive any more.
        std::future::pending::<()>().await;
    }
}

/// What a forced exit works with.
struct ForcedExitContext {
    cancellation: Cancellation,
    shared: Arc<Shared>,
    /// How the forced exit reports.
    format: OutputFormat,
    /// How hints name the run's environment and project.
    invocation: Invocation,
    gate: Arc<ResultGate>,
}

struct Watcher<Source> {
    signals: Source,
    sender: watch::Sender<bool>,
    exit: ForcedExitContext,
}

async fn watch_signals(mut watcher: Watcher<impl SignalSource>) {
    if !watcher.signals.next().await {
        return;
    }
    // Stops new work and asks every running provider to stop the operation
    // in flight.
    watcher.exit.cancellation.stop();
    watcher.sender.send_replace(true);
    if watcher.exit.shared.lock().is_some() {
        emit_stderr!(
            "Interrupted: providers were asked to stop; no new resource will be started, what \
             is in flight will be recorded and the lock released. Interrupt again to exit \
             immediately."
        );
    } else {
        emit_stderr!(
            "Interrupted: stopping; providers were asked to stop. Interrupt again to exit \
             immediately."
        );
    }

    if !watcher.signals.next().await {
        return;
    }
    exit_now(&watcher.exit).await;
}

/// Whether records being written finished before a forced exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Recordings {
    Finished,
    StillWriting,
}

/// What a forced exit could do about the lock.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ExitLock {
    /// No lock was held or requested.
    None,
    /// The held lock was released.
    Released(String),
    /// The held lock could not be released in time.
    NotReleased(String),
    /// The lock was being requested; it may have been acquired.
    MaybeAcquired(String),
}

/// What a forced exit found.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ForcedExit {
    recordings: Recordings,
    lock: ExitLock,
}

fn review_plan(invocation: &Invocation) -> String {
    format!(
        "A resource in flight may have changed without being recorded; review `{}`.",
        invocation.command("plan")
    )
}

/// The error a forced exit reports, with the lock it concerns.
fn forced_exit_error(forced: &ForcedExit, invocation: &Invocation) -> CliError {
    let recording = match forced.recordings {
        Recordings::Finished => "",
        Recordings::StillWriting => "; a record was still being written",
    };
    let status = |identifier: &String, released| {
        Some(LockStatus {
            identifier: identifier.clone(),
            released,
        })
    };
    let (outcome, help, lock) = match &forced.lock {
        ExitLock::None => (String::new(), review_plan(invocation), None),
        ExitLock::Released(identifier) => (
            format!("; lock {identifier} was released"),
            review_plan(invocation),
            status(identifier, true),
        ),
        ExitLock::NotReleased(identifier) => (
            format!("; lock {identifier} was NOT released"),
            format!(
                "After checking no run is active, release it with `{}`. {}",
                invocation.command(&format!("unlock {identifier}")),
                review_plan(invocation)
            ),
            status(identifier, false),
        ),
        ExitLock::MaybeAcquired(identifier) => (
            format!("; lock {identifier} may have been acquired"),
            format!(
                "If `{}` shows lock {identifier}, release it with `{}`.",
                invocation.command("unlock"),
                invocation.command(&format!("unlock {identifier}"))
            ),
            status(identifier, false),
        ),
    };
    let message = format!(
        "interrupted again: exited immediately after killing every provider process\
         {recording}{outcome}"
    );
    let error =
        CliError::infrastructure(message, Some(help), InfrastructureFailureKind::Interrupted);
    match lock {
        Some(lock) => error.with_lock(lock),
        None => error,
    }
}

/// Second interrupt: stop everything, release what can be released within
/// a bound, report once and exit. Returns only when the command already
/// finished and reports its own outcome.
async fn exit_now(context: &ForcedExitContext) {
    // Kill every provider process first, so nothing keeps changing
    // infrastructure once this process is gone; this also removes their
    // socket directories.
    context.cancellation.terminate_providers();
    let cancellation = context.cancellation.clone();
    // The wait blocks on a condition variable, so it runs off the runtime's
    // worker threads while the record in flight is written on them.
    let recordings = match tokio::task::spawn_blocking(move || {
        cancellation.wait_for_recordings(RECORDING_BOUND)
    })
    .await
    {
        Ok(true) => Recordings::Finished,
        Ok(false) | Err(_) => Recordings::StillWriting,
    };
    let lock = match context.shared.lock() {
        None => ExitLock::None,
        Some((phase, held)) => {
            let released =
                tokio::time::timeout(RELEASE_BOUND, held.store.unlock(&held.tenant, &held.lock))
                    .await;
            let identifier = held.lock.lock_identifier.clone();
            match (phase, released) {
                // The request may commit after the release was attempted.
                (LockPhase::Acquiring, _) => ExitLock::MaybeAcquired(identifier),
                (LockPhase::Held, Ok(Ok(()))) => ExitLock::Released(identifier),
                (LockPhase::Held, Ok(Err(_)) | Err(_)) => ExitLock::NotReleased(identifier),
            }
        }
    };
    let error = forced_exit_error(&ForcedExit { recordings, lock }, &context.invocation);
    if !context.gate.claim_for_exit() {
        // The command finished meanwhile and is reporting its own outcome.
        return;
    }
    report_forced_exit(&error, context.format);
    std::process::exit(EXIT_INTERRUPTED);
}

/// Write the forced exit's outcome directly (the asynchronous event
/// renderer would not run again before the process exits): one line on
/// standard error, rendered like any other event in JSON mode, and in JSON
/// mode the one error envelope on standard output. Everything is redacted
/// and flushed.
fn report_forced_exit(error: &CliError, format: OutputFormat) {
    let line = match error.help() {
        Some(help) => format!("{error}. {help}"),
        None => error.to_string(),
    };
    if !format.is_json() {
        cuenv_events::eprintln_redacted(&line);
        return;
    }
    let event = cuenv_events::CuenvEvent::new(
        cuenv_events::correlation_id(),
        cuenv_events::EventSource::new("cuenv::infrastructure"),
        cuenv_events::EventCategory::Output(cuenv_events::OutputEvent::Stderr {
            content: cuenv_events::redact(&line),
        }),
    );
    let mut standard_error = std::io::stderr().lock();
    if let Err(write_error) = cuenv_events::JsonRenderer::new()
        .render_to_writer(&event, &mut standard_error)
        .and_then(|()| standard_error.flush())
    {
        // Standard error itself failed; the envelope below still carries
        // the outcome.
        tracing::debug!(%write_error, "failed to write the forced exit event");
    }
    drop(standard_error);
    let written = serde_json::to_string(&crate::cli::error_envelope(error))
        .map_err(std::io::Error::other)
        .and_then(|envelope| {
            let mut standard_output = std::io::stdout().lock();
            writeln!(standard_output, "{envelope}")?;
            standard_output.flush()
        });
    if let Err(write_error) = written {
        // Standard output is gone (closed pipe, full disk); standard error
        // is the only channel left to say so.
        cuenv_events::eprintln_redacted(&format!(
            "error: could not write the JSON error envelope to standard output: {write_error}"
        ));
    }
}

/// Interrupt signal streams, installed when created: SIGINT, SIGTERM,
/// SIGHUP and SIGQUIT on unix, Ctrl-C and Ctrl-Break on Windows.
struct Signals {
    #[cfg(unix)]
    interrupt: tokio::signal::unix::Signal,
    #[cfg(unix)]
    terminate: tokio::signal::unix::Signal,
    #[cfg(unix)]
    hang_up: tokio::signal::unix::Signal,
    #[cfg(unix)]
    quit: tokio::signal::unix::Signal,
    #[cfg(windows)]
    interrupt: tokio::signal::windows::CtrlC,
    #[cfg(windows)]
    terminate: tokio::signal::windows::CtrlBreak,
}

fn installation_failure(error: &std::io::Error) -> CliError {
    CliError::infrastructure(
        format!("cannot install interrupt handlers: {error}"),
        None,
        InfrastructureFailureKind::Failed,
    )
}

impl Signals {
    #[cfg(unix)]
    fn install() -> Result<Self, CliError> {
        use tokio::signal::unix::{SignalKind, signal};
        let install = |kind: SignalKind| signal(kind).map_err(|error| installation_failure(&error));
        Ok(Self {
            interrupt: install(SignalKind::interrupt())?,
            terminate: install(SignalKind::terminate())?,
            hang_up: install(SignalKind::hangup())?,
            quit: install(SignalKind::quit())?,
        })
    }

    #[cfg(windows)]
    fn install() -> Result<Self, CliError> {
        use tokio::signal::windows::{ctrl_break, ctrl_c};
        Ok(Self {
            interrupt: ctrl_c().map_err(|error| installation_failure(&error))?,
            terminate: ctrl_break().map_err(|error| installation_failure(&error))?,
        })
    }
}

impl SignalSource for Signals {
    #[cfg(unix)]
    async fn next(&mut self) -> bool {
        tokio::select! {
            received = self.interrupt.recv() => received.is_some(),
            received = self.terminate.recv() => received.is_some(),
            received = self.hang_up.recv() => received.is_some(),
            received = self.quit.recv() => received.is_some(),
        }
    }

    #[cfg(windows)]
    async fn next(&mut self) -> bool {
        tokio::select! {
            received = self.interrupt.recv() => received.is_some(),
            received = self.terminate.recv() => received.is_some(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lock_of(error: &CliError) -> Option<&LockStatus> {
        match error {
            CliError::Infrastructure { lock, .. } => lock.as_ref(),
            _ => None,
        }
    }

    #[test]
    fn a_forced_exit_during_acquisition_names_the_lock_it_may_hold() {
        let error = forced_exit_error(
            &ForcedExit {
                recordings: Recordings::Finished,
                lock: ExitLock::MaybeAcquired("abc".to_string()),
            },
            &Invocation::default(),
        );
        assert_eq!(crate::cli::exit_code_for(&error), EXIT_INTERRUPTED);
        assert!(
            error
                .to_string()
                .contains("lock abc may have been acquired")
        );
        assert!(
            error
                .help()
                .unwrap()
                .contains("release it with `cuenv infrastructure unlock abc`")
        );
        assert_eq!(
            lock_of(&error),
            Some(&LockStatus {
                identifier: "abc".to_string(),
                released: false
            })
        );
    }

    #[test]
    fn a_forced_exit_never_claims_an_unreleased_lock_was_released() {
        let not_released = forced_exit_error(
            &ForcedExit {
                recordings: Recordings::StillWriting,
                lock: ExitLock::NotReleased("abc".to_string()),
            },
            &Invocation::default(),
        );
        let message = not_released.to_string();
        assert!(message.contains("lock abc was NOT released"), "{message}");
        assert!(
            message.contains("a record was still being written"),
            "{message}"
        );
        assert!(!lock_of(&not_released).unwrap().released);

        let released = forced_exit_error(
            &ForcedExit {
                recordings: Recordings::Finished,
                lock: ExitLock::Released("abc".to_string()),
            },
            &Invocation::default(),
        );
        assert!(lock_of(&released).unwrap().released);

        let unlocked = forced_exit_error(
            &ForcedExit {
                recordings: Recordings::Finished,
                lock: ExitLock::None,
            },
            &Invocation::default(),
        );
        assert!(lock_of(&unlocked).is_none());
        let envelope = serde_json::to_value(crate::cli::error_envelope(&unlocked)).unwrap();
        assert_eq!(envelope["error"]["code"], "infrastructure_interrupted");
        assert!(envelope["error"]["help"].is_string());
    }
}
