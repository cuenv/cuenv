//! Per-service supervisor.
//!
//! Manages the spawn-probe-restart-shutdown lifecycle for a single service.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Child;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

#[cfg(unix)]
use libc;

use cuenv_core::manifest::Service;
use cuenv_events::{
    emit_service_failed, emit_service_output, emit_service_ready, emit_service_ready_timeout,
    emit_service_restarting, emit_service_starting, emit_service_stopped, emit_service_stopping,
};

use crate::control::{ManualControlRequest, wait_for_control_request};
use crate::duration::parse_duration;
use crate::lifecycle::ServiceLifecycle;
use crate::probes::{self, ProbeLoopResult, log::LogProbe};
use crate::process::ServiceProcess;
use crate::session::{ServiceState, SessionManager};
use crate::watcher::{ServiceWatcher, WatchEvent};

/// Result of supervisor execution.
#[derive(Debug)]
pub enum SupervisorResult {
    /// Service stopped normally (cuenv down / Ctrl-C).
    Stopped,
    /// Service failed and exhausted restart attempts.
    Failed(String),
}

/// Signal sent from the supervisor to the controller during startup.
///
/// The controller waits for a non-`Pending` value before proceeding
/// to the next dependency group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadinessOutcome {
    /// Still starting — not yet determined.
    Pending,
    /// Readiness probe passed.
    Ready,
    /// Service failed fatally during startup (exhausted restarts, probe
    /// error, spawn failure). Contains the error message.
    Failed(String),
}

/// Exponential backoff calculator.
struct ExponentialBackoff {
    initial: Duration,
    max: Duration,
    factor: f64,
    current: Duration,
}

impl ExponentialBackoff {
    fn new(initial: Duration, max: Duration, factor: f64) -> Self {
        Self {
            initial,
            max,
            factor,
            current: initial,
        }
    }

    fn next_delay(&mut self) -> Duration {
        let delay = self.current;
        let next = Duration::from_secs_f64(self.current.as_secs_f64() * self.factor);
        self.current = next.min(self.max);
        delay
    }

    fn reset(&mut self) {
        self.current = self.initial;
    }
}

/// Configuration for constructing a [`ServiceSupervisor`].
pub struct SupervisorConfig {
    /// Service name.
    pub name: String,
    /// Service definition from CUE.
    pub service: Service,
    /// Project root directory.
    pub project_root: PathBuf,
    /// Shared session manager.
    pub session: Arc<SessionManager>,
}

/// Supervisor for a single service.
pub struct ServiceSupervisor {
    name: String,
    service: Service,
    project_root: PathBuf,
    session: Arc<SessionManager>,
}

/// Parameters for a state update.
#[derive(Clone, Copy)]
pub struct StateUpdate<'a> {
    /// New lifecycle state.
    pub lifecycle: ServiceLifecycle,
    /// Process ID (if running).
    pub pid: Option<u32>,
    /// Exit code (if exited).
    pub exit_code: Option<i32>,
    /// Error message (if failed).
    pub error: Option<&'a str>,
}

#[derive(Clone, Copy)]
enum RestartTrigger {
    Manual,
    Watch,
}

impl RestartTrigger {
    fn reason(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Watch => "watch",
        }
    }

    fn counts_toward_budget(self) -> bool {
        matches!(self, Self::Watch)
    }
}

#[derive(Clone, Copy)]
struct RestartStep {
    trigger: RestartTrigger,
    current_attempt: u32,
}

enum ServiceWait {
    Exited(std::io::Result<std::process::ExitStatus>),
    Shutdown,
    Restart(RestartTrigger),
    /// Send SIGHUP to the running process (watch `on: sync`).
    Sync,
    Stop,
}

fn abort_output_handles(
    stdout_handle: Option<tokio::task::JoinHandle<()>>,
    stderr_handle: Option<tokio::task::JoinHandle<()>>,
) {
    if let Some(handle) = stdout_handle {
        handle.abort();
    }
    if let Some(handle) = stderr_handle {
        handle.abort();
    }
}

/// Send SIGHUP to a running service process for in-place config reload.
///
/// On non-Unix platforms, logs a warning since SIGHUP is not available.
fn send_sighup(pid: u32, service_name: &str) {
    #[cfg(unix)]
    {
        let Ok(raw_pid) = i32::try_from(pid) else {
            warn!(service = %service_name, pid, "PID does not fit platform pid_t; skipping SIGHUP");
            return;
        };
        #[expect(
            unsafe_code,
            reason = "kill(pid, SIGHUP) sends a config-reload signal to the supervised process"
        )]
        // SAFETY: SIGHUP is sent to the supervised child process, which was
        // spawned by this supervisor and whose PID is still valid at this point
        // (we hold `child` which owns the process handle).
        let result = unsafe { libc::kill(raw_pid, libc::SIGHUP) };
        if result != 0 {
            warn!(
                service = %service_name,
                pid,
                error = %std::io::Error::last_os_error(),
                "Failed to send SIGHUP"
            );
        } else {
            debug!(service = %service_name, pid, "Sent SIGHUP for config reload");
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        warn!(
            service = %service_name,
            "watch.on: sync is not supported on this platform; SIGHUP unavailable"
        );
    }
}

impl ServiceSupervisor {
    /// Create a new supervisor from configuration.
    #[must_use]
    pub fn new(config: SupervisorConfig) -> Self {
        Self {
            name: config.name,
            service: config.service,
            project_root: config.project_root,
            session: config.session,
        }
    }

    /// Run the supervisor loop.
    ///
    /// Spawns the service process, runs readiness probes, handles restarts,
    /// and responds to file watcher events. Returns when the service reaches
    /// a terminal state or the shutdown token is cancelled.
    ///
    /// The `readiness_tx` watch channel is used to signal the controller
    /// when this service becomes ready or fails permanently during startup.
    pub async fn run(
        &self,
        shutdown: CancellationToken,
        readiness_tx: watch::Sender<ReadinessOutcome>,
    ) -> SupervisorResult {
        let restart_policy = self.service.restart.as_ref();
        let mode = restart_policy
            .and_then(|r| r.mode.as_deref())
            .unwrap_or("onFailure");
        let max_restarts = restart_policy.and_then(|r| r.max_restarts).unwrap_or(5);
        let window = restart_policy
            .and_then(|r| r.window.as_deref())
            .and_then(|w| parse_duration(w).ok())
            .unwrap_or(Duration::from_secs(60));

        let backoff_config = restart_policy.and_then(|r| r.backoff.as_ref());
        let mut backoff = ExponentialBackoff::new(
            backoff_config
                .and_then(|b| b.initial.as_deref())
                .and_then(|s| parse_duration(s).ok())
                .unwrap_or(Duration::from_secs(1)),
            backoff_config
                .and_then(|b| b.max.as_deref())
                .and_then(|s| parse_duration(s).ok())
                .unwrap_or(Duration::from_secs(30)),
            backoff_config.and_then(|b| b.factor).unwrap_or(2.0),
        );

        let mut restart_history: VecDeque<Instant> = VecDeque::new();
        let mut attempt: u32 = 0;
        let mut has_signaled_readiness = false;

        // Set up file watcher if configured
        let (watch_tx, mut watch_rx) = mpsc::channel::<WatchEvent>(16);
        let _watcher = self.service.watch.as_ref().and_then(|w| {
            let debounce = w
                .debounce
                .as_deref()
                .and_then(|d| parse_duration(d).ok())
                .unwrap_or(Duration::from_millis(200));
            let ignore = w.ignore.clone().unwrap_or_default();
            ServiceWatcher::start(&self.project_root, &w.paths, &ignore, debounce, watch_tx).ok()
        });

        loop {
            if shutdown.is_cancelled() {
                return SupervisorResult::Stopped;
            }

            // Check restart budget
            let now = Instant::now();
            restart_history.retain(|t| now.duration_since(*t) < window);
            if restart_history.len() >= max_restarts as usize && attempt > 0 {
                let msg = format!(
                    "exceeded {max_restarts} restarts within {}s",
                    window.as_secs()
                );
                emit_service_failed!(&self.name, &msg);
                self.log_state_update(StateUpdate {
                    lifecycle: ServiceLifecycle::Failed,
                    pid: None,
                    exit_code: None,
                    error: Some(&msg),
                });
                if !has_signaled_readiness {
                    let _ = readiness_tx.send(ReadinessOutcome::Failed(msg.clone()));
                }
                return SupervisorResult::Failed(msg);
            }

            // Spawn the service process
            emit_service_starting!(&self.name, self.process().command_display());
            self.log_state_update(StateUpdate {
                lifecycle: ServiceLifecycle::Starting,
                pid: None,
                exit_code: None,
                error: None,
            });

            let spawn_result = self.process().spawn().await;
            let mut child = match spawn_result {
                Ok(c) => c,
                Err(e) => {
                    let msg = format!("failed to spawn: {e}");
                    emit_service_failed!(&self.name, &msg);
                    self.log_state_update(StateUpdate {
                        lifecycle: ServiceLifecycle::Failed,
                        pid: None,
                        exit_code: None,
                        error: Some(&msg),
                    });
                    if !has_signaled_readiness {
                        let _ = readiness_tx.send(ReadinessOutcome::Failed(msg.clone()));
                    }
                    return SupervisorResult::Failed(msg);
                }
            };

            let pid = child.id();
            self.log_state_update(StateUpdate {
                lifecycle: ServiceLifecycle::Starting,
                pid,
                exit_code: None,
                error: None,
            });

            // Set up output streaming
            let stdout = child.stdout.take();
            let stderr = child.stderr.take();

            // Create log probe if needed
            let log_probe = self.service.readiness.as_ref().and_then(|r| match r {
                cuenv_core::manifest::Readiness::Log(l) => {
                    let source = l.source.as_deref().unwrap_or("either");
                    LogProbe::new(&l.pattern, source).ok()
                }
                _ => None,
            });

            let log_probe = Arc::new(log_probe);

            // Stream stdout
            let name_clone = self.name.clone();
            let session_clone = Arc::clone(&self.session);
            let log_probe_stdout = Arc::clone(&log_probe);
            let stdout_handle = stdout.map(|stdout| {
                tokio::spawn(async move {
                    let reader = BufReader::new(stdout);
                    let mut lines = reader.lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        emit_service_output!(&name_clone, "stdout", &line);
                        let _ = session_clone.append_log(&name_clone, &line);
                        if let Some(ref probe) = *log_probe_stdout {
                            probe.feed_line(&line, "stdout").await;
                        }
                    }
                })
            });

            // Stream stderr
            let name_clone = self.name.clone();
            let session_clone = Arc::clone(&self.session);
            let log_probe_stderr = Arc::clone(&log_probe);
            let stderr_handle = stderr.map(|stderr| {
                tokio::spawn(async move {
                    let reader = BufReader::new(stderr);
                    let mut lines = reader.lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        emit_service_output!(&name_clone, "stderr", &line);
                        let _ = session_clone.append_log(&name_clone, &line);
                        if let Some(ref probe) = *log_probe_stderr {
                            probe.feed_line(&line, "stderr").await;
                        }
                    }
                })
            });

            // Run readiness probe.
            // For log probes, reuse the shared `log_probe` instance that receives
            // fed lines from the output handlers — `create_probe` would construct a
            // separate instance whose `matched` flag is never set.
            let ready = if let Some(ref readiness) = self.service.readiness {
                let probe_result = if let Some(ref shared_probe) = *log_probe {
                    // Log probe: build config from common fields, run against the shared instance
                    match probes::build_probe_config(
                        readiness
                            .common_fields()
                            .and_then(|c| c.interval.as_deref()),
                        readiness.common_fields().and_then(|c| c.timeout.as_deref()),
                        readiness
                            .common_fields()
                            .and_then(|c| c.initial_delay.as_deref()),
                    ) {
                        Ok(config) => Ok(probes::run_probe_loop(shared_probe, &config).await),
                        Err(e) => Err(e),
                    }
                } else {
                    // Non-log probes: create_probe is fine
                    match probes::create_probe(readiness) {
                        Ok((probe, config)) => {
                            Ok(probes::run_probe_loop(probe.as_ref(), &config).await)
                        }
                        Err(e) => Err(e),
                    }
                };

                match probe_result {
                    Ok(ProbeLoopResult::Ready { after_ms }) => {
                        emit_service_ready!(&self.name, after_ms);
                        true
                    }
                    Ok(ProbeLoopResult::TimedOut { after_ms }) => {
                        let msg = format!("readiness probe timed out after {after_ms}ms");
                        emit_service_ready_timeout!(&self.name, after_ms);
                        self.log_state_update(StateUpdate {
                            lifecycle: ServiceLifecycle::Failed,
                            pid,
                            exit_code: None,
                            error: Some(&msg),
                        });
                        if !has_signaled_readiness {
                            let _ = readiness_tx.send(ReadinessOutcome::Failed(msg.clone()));
                        }
                        let _ = self.process().stop(&mut child).await;
                        return SupervisorResult::Failed(msg);
                    }
                    Ok(ProbeLoopResult::Fatal(msg)) => {
                        emit_service_failed!(&self.name, &msg);
                        self.log_state_update(StateUpdate {
                            lifecycle: ServiceLifecycle::Failed,
                            pid,
                            exit_code: None,
                            error: Some(&msg),
                        });
                        if !has_signaled_readiness {
                            let _ = readiness_tx.send(ReadinessOutcome::Failed(msg.clone()));
                        }
                        let _ = self.process().stop(&mut child).await;
                        return SupervisorResult::Failed(msg);
                    }
                    Err(e) => {
                        let msg = format!("probe creation failed: {e}");
                        emit_service_failed!(&self.name, &msg);
                        self.log_state_update(StateUpdate {
                            lifecycle: ServiceLifecycle::Failed,
                            pid,
                            exit_code: None,
                            error: Some(&msg),
                        });
                        if !has_signaled_readiness {
                            let _ = readiness_tx.send(ReadinessOutcome::Failed(msg.clone()));
                        }
                        let _ = self.process().stop(&mut child).await;
                        return SupervisorResult::Failed(msg);
                    }
                }
            } else {
                // No readiness probe — consider immediately ready
                emit_service_ready!(&self.name, 0_u64);
                true
            };

            if ready {
                backoff.reset();
                self.log_state_update(StateUpdate {
                    lifecycle: ServiceLifecycle::Ready,
                    pid,
                    exit_code: None,
                    error: None,
                });
                if !has_signaled_readiness {
                    let _ = readiness_tx.send(ReadinessOutcome::Ready);
                    has_signaled_readiness = true;
                }
            }

            let watch_on = self
                .service
                .watch
                .as_ref()
                .and_then(|w| w.on.as_deref())
                .unwrap_or("restart");

            // Wait for process exit, shutdown, file watcher, or a manual
            // control request queued by `cuenv down` / `cuenv restart`.
            let wait = tokio::select! {
                status = child.wait() => ServiceWait::Exited(status),
                () = shutdown.cancelled() => ServiceWait::Shutdown,
                Some(watch_event) = watch_rx.recv() => {
                    let changed: Vec<String> = watch_event.paths.iter()
                        .map(|p| p.to_string_lossy().into_owned())
                        .collect();
                    cuenv_events::emit_service_watch!(&self.name, &changed);
                    if watch_on == "sync" {
                        ServiceWait::Sync
                    } else {
                        ServiceWait::Restart(RestartTrigger::Watch)
                    }
                }
                () = wait_for_control_request(
                    self.session.as_ref(),
                    &self.name,
                    ManualControlRequest::Restart,
                ) => {
                    ServiceWait::Restart(RestartTrigger::Manual)
                }
                () = wait_for_control_request(
                    self.session.as_ref(),
                    &self.name,
                    ManualControlRequest::Stop,
                ) => ServiceWait::Stop,
            };

            let exit_status = match wait {
                ServiceWait::Exited(status) => {
                    abort_output_handles(stdout_handle, stderr_handle);
                    status
                }
                ServiceWait::Shutdown => {
                    self.stop_running_process(&mut child).await;
                    abort_output_handles(stdout_handle, stderr_handle);
                    return SupervisorResult::Stopped;
                }
                ServiceWait::Stop => {
                    self.stop_running_process(&mut child).await;
                    abort_output_handles(stdout_handle, stderr_handle);
                    return SupervisorResult::Stopped;
                }
                ServiceWait::Sync => {
                    // Send SIGHUP to the running process for in-place config reload.
                    // No restart counter increment — the process keeps running.
                    if let Some(pid) = child.id() {
                        send_sighup(pid, &self.name);
                    }
                    continue;
                }
                ServiceWait::Restart(trigger) => {
                    attempt = self
                        .restart_running_process(
                            &mut child,
                            RestartStep {
                                trigger,
                                current_attempt: attempt,
                            },
                            &mut restart_history,
                        )
                        .await;
                    abort_output_handles(stdout_handle, stderr_handle);
                    continue;
                }
            };

            let exit_code = exit_status.ok().and_then(|s| s.code());
            emit_service_stopped!(&self.name, exit_code);

            // Decide whether to restart
            let should_restart = match mode {
                "never" => false,
                "always" => true,
                "unlessStopped" => !shutdown.is_cancelled(),
                _ => {
                    // "onFailure" (default)
                    exit_code.is_none_or(|c| c != 0)
                }
            };

            if should_restart && !shutdown.is_cancelled() {
                restart_history.push_back(Instant::now());
                attempt += 1;
                emit_service_restarting!(&self.name, "crashed", attempt);
                self.log_state_update(StateUpdate {
                    lifecycle: ServiceLifecycle::Restarting,
                    pid: None,
                    exit_code: None,
                    error: None,
                });

                let delay = backoff.next_delay();
                debug!(
                    service = %self.name,
                    attempt,
                    delay_ms = delay.as_millis(),
                    "Backing off before restart"
                );
                tokio::time::sleep(delay).await;
            } else {
                if exit_code.is_none_or(|c| c != 0) {
                    let msg = format!(
                        "exited with code {}",
                        exit_code.map_or("signal".to_string(), |c| c.to_string())
                    );
                    emit_service_failed!(&self.name, &msg);
                    self.log_state_update(StateUpdate {
                        lifecycle: ServiceLifecycle::Failed,
                        pid: None,
                        exit_code,
                        error: Some(&msg),
                    });
                    if !has_signaled_readiness {
                        let _ = readiness_tx.send(ReadinessOutcome::Failed(msg.clone()));
                    }
                    return SupervisorResult::Failed(msg);
                }
                self.log_state_update(StateUpdate {
                    lifecycle: ServiceLifecycle::Stopped,
                    pid: None,
                    exit_code,
                    error: None,
                });
                return SupervisorResult::Stopped;
            }
        }
    }

    async fn stop_running_process(&self, child: &mut Child) {
        emit_service_stopping!(&self.name);
        self.log_state_update(StateUpdate {
            lifecycle: ServiceLifecycle::Stopping,
            pid: child.id(),
            exit_code: None,
            error: None,
        });
        let exit_code = self.process().stop(child).await;
        self.log_state_update(StateUpdate {
            lifecycle: ServiceLifecycle::Stopped,
            pid: None,
            exit_code,
            error: None,
        });
    }

    async fn restart_running_process(
        &self,
        child: &mut Child,
        step: RestartStep,
        restart_history: &mut VecDeque<Instant>,
    ) -> u32 {
        let next_attempt = step.current_attempt + 1;
        emit_service_restarting!(&self.name, step.trigger.reason(), next_attempt);
        self.log_state_update(StateUpdate {
            lifecycle: ServiceLifecycle::Restarting,
            pid: None,
            exit_code: None,
            error: None,
        });
        let _ = self.process().stop(child).await;
        if step.trigger.counts_toward_budget() {
            restart_history.push_back(Instant::now());
            return next_attempt;
        }
        step.current_attempt
    }

    fn process(&self) -> ServiceProcess<'_> {
        ServiceProcess::new(&self.name, &self.service, &self.project_root)
    }

    /// Update state with a warning log on failure (instead of silently swallowing).
    fn log_state_update(&self, update: StateUpdate<'_>) {
        if let Err(e) = self.update_state(&update) {
            warn!(
                service = %self.name,
                lifecycle = %update.lifecycle,
                error = %e,
                "Failed to persist service state"
            );
        }
    }

    fn update_state(&self, update: &StateUpdate<'_>) -> crate::Result<()> {
        // Try to read existing state to preserve accumulated fields
        let existing = self.session.read_service(&self.name).ok();

        let state = ServiceState {
            name: self.name.clone(),
            lifecycle: update.lifecycle,
            pid: update.pid,
            started_at: existing
                .as_ref()
                .and_then(|s| s.started_at)
                .or(Some(chrono::Utc::now())),
            ready_at: if update.lifecycle == ServiceLifecycle::Ready {
                Some(chrono::Utc::now())
            } else {
                existing.as_ref().and_then(|s| s.ready_at)
            },
            restarts: existing.as_ref().map_or(0, |s| {
                if update.lifecycle == ServiceLifecycle::Restarting {
                    s.restarts + 1
                } else {
                    s.restarts
                }
            }),
            exit_code: update.exit_code,
            error: update.error.map(String::from),
            ports: self.service.ports.clone(),
        };
        self.session.update_service(&state)
    }
}
