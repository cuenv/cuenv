//! Interrupt ownership for `cuenv infrastructure`.
//!
//! The command owns SIGINT and SIGTERM for its whole run: `main` does not
//! race it on Ctrl-C (see `InterruptPolicy` in `main.rs`), and the handlers
//! are installed before evaluation, so they are in place long before the
//! state lock is taken.
//!
//! - First signal: request cancellation. Work that has not changed anything
//!   (evaluation, planning, the confirmation prompt, state reads) stops;
//!   an apply stops starting new resources, records what the resource in
//!   flight returns, and releases the lock.
//! - Second signal: exit at once, after trying to release a held lock for
//!   about two seconds and writing the lock identifier with a direct stderr
//!   write (the asynchronous renderer may never run again).

use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use cuenv_events::emit_stderr;
use cuenv_infrastructure::{Cancellation, StateLock, StateStore, TenantKey};
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::cli::{CliError, InfrastructureFailureKind};

/// Exit code after a second interrupt (128 + SIGINT).
const EXIT_INTERRUPTED: i32 = 130;

/// How long a second interrupt waits for the lock to be released.
const RELEASE_BOUND: Duration = Duration::from_secs(2);

/// A lock the command holds, so a second interrupt can try to release it.
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

/// What the signal watcher shares with the command.
#[derive(Debug, Default)]
struct Shared {
    held: Mutex<Option<HeldLock>>,
}

impl Shared {
    fn held(&self) -> Option<HeldLock> {
        self.held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn set(&self, held: Option<HeldLock>) {
        *self.held.lock().unwrap_or_else(PoisonError::into_inner) = held;
    }
}

/// The command's claim on SIGINT and SIGTERM. Dropping it aborts the
/// watcher task.
#[derive(Debug)]
pub(super) struct Interrupts {
    cancellation: Cancellation,
    requested: watch::Receiver<bool>,
    shared: Arc<Shared>,
    watcher: JoinHandle<()>,
}

impl Interrupts {
    /// Install the signal handlers and start watching.
    ///
    /// # Errors
    ///
    /// Returns an error when the handlers cannot be installed; the command
    /// refuses to run rather than run without clean interrupt handling.
    pub(super) fn claim() -> Result<Self, CliError> {
        let signals = Signals::install()?;
        let cancellation = Cancellation::default();
        let (sender, requested) = watch::channel(false);
        let shared = Arc::new(Shared::default());
        let watcher = tokio::spawn(watch_signals(Watcher {
            signals,
            cancellation: cancellation.clone(),
            sender,
            shared: Arc::clone(&shared),
        }));
        Ok(Self {
            cancellation,
            requested,
            shared,
            watcher,
        })
    }

    /// The cancellation handed to the engine.
    #[must_use]
    pub(super) const fn cancellation(&self) -> &Cancellation {
        &self.cancellation
    }

    /// Whether an interrupt was received.
    #[must_use]
    pub(super) fn is_requested(&self) -> bool {
        self.cancellation.is_requested()
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

    /// Record the lock the command now holds.
    pub(super) fn hold(&self, held: HeldLock) {
        self.shared.set(Some(held));
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

/// The error for work stopped by an interrupt.
#[must_use]
pub(super) fn interrupted() -> CliError {
    CliError::infrastructure(
        "interrupted; no change was applied",
        None,
        InfrastructureFailureKind::Failed,
    )
}

async fn wait_for_request(requested: &mut watch::Receiver<bool>) {
    if requested.wait_for(|requested| *requested).await.is_err() {
        // The watcher is gone, so no interrupt can arrive any more.
        std::future::pending::<()>().await;
    }
}

struct Watcher {
    signals: Signals,
    cancellation: Cancellation,
    sender: watch::Sender<bool>,
    shared: Arc<Shared>,
}

async fn watch_signals(mut watcher: Watcher) {
    if !watcher.signals.next().await {
        return;
    }
    // Pass 2 (Terraform semantics): ask running providers to stop their
    // in-flight operations here as well, once the engine offers it.
    watcher.cancellation.request();
    watcher.sender.send_replace(true);
    if watcher.shared.held().is_some() {
        emit_stderr!(
            "Interrupted: no new resources will be started; recording what is in flight and \
             releasing the lock. Interrupt again to exit immediately."
        );
    } else {
        emit_stderr!("Interrupted: stopping. Interrupt again to exit immediately.");
    }

    if !watcher.signals.next().await {
        return;
    }
    exit_now(watcher.shared.held()).await;
}

/// Second interrupt: release what can be released within a bound, tell the
/// operator what remains, and exit.
async fn exit_now(held: Option<HeldLock>) {
    // Pass 2: kill every provider process synchronously first, once the
    // engine offers it, so nothing keeps changing infrastructure after exit.
    let message = match held {
        None => "Exiting immediately.".to_string(),
        Some(held) => {
            let released =
                tokio::time::timeout(RELEASE_BOUND, held.store.unlock(&held.tenant, &held.lock))
                    .await;
            match released {
                Ok(Ok(())) => format!(
                    "Exiting immediately; released lock {}. A resource in flight may have \
                     changed without being recorded; review `cuenv infrastructure plan`.",
                    held.lock.lock_identifier
                ),
                Ok(Err(_)) | Err(_) => format!(
                    "Exiting immediately; the lock was NOT released. After checking no run is \
                     active, release it with `cuenv infrastructure unlock {}`.",
                    held.lock.lock_identifier
                ),
            }
        }
    };
    // The event renderer runs asynchronously and would not print before the
    // process exits; write directly (and redacted) instead.
    cuenv_events::eprintln_redacted(&message);
    std::process::exit(EXIT_INTERRUPTED);
}

/// Interrupt signal streams, installed when created (SIGINT and SIGTERM on
/// unix, Ctrl-C and Ctrl-Break on Windows).
struct Signals {
    #[cfg(unix)]
    interrupt: tokio::signal::unix::Signal,
    #[cfg(unix)]
    terminate: tokio::signal::unix::Signal,
    #[cfg(windows)]
    interrupt: tokio::signal::windows::CtrlC,
    #[cfg(windows)]
    terminate: tokio::signal::windows::CtrlBreak,
}

fn installation_failure(error: &std::io::Error) -> CliError {
    CliError::other(format!("cannot install interrupt handlers: {error}"))
}

impl Signals {
    #[cfg(unix)]
    fn install() -> Result<Self, CliError> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(Self {
            interrupt: signal(SignalKind::interrupt())
                .map_err(|error| installation_failure(&error))?,
            terminate: signal(SignalKind::terminate())
                .map_err(|error| installation_failure(&error))?,
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

    /// Wait for the next signal. Returns `false` when no more can arrive.
    async fn next(&mut self) -> bool {
        tokio::select! {
            received = self.interrupt.recv() => received.is_some(),
            received = self.terminate.recv() => received.is_some(),
        }
    }
}
