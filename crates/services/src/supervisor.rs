//! Per-service supervisor.
//!
//! Manages the spawn-probe-restart-shutdown lifecycle for a single service.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use cuenv_core::manifest::Service;
use cuenv_events::{
    emit_service_failed, emit_service_output, emit_service_ready, emit_service_ready_timeout,
    emit_service_restarting, emit_service_starting, emit_service_stopped, emit_service_stopping,
};

use crate::duration::parse_duration;
use crate::lifecycle::ServiceLifecycle;
use crate::probes::{self, ProbeLoopResult, log::LogProbe};
use crate::session::{ServiceState, SessionManager};
use crate::watcher::{ServiceWatcher, WatchEvent};

/// Wrap the invocation with `cuenv __supervise` on platforms that need
/// it for orphan prevention.
///
/// On Linux the child is already covered by `PR_SET_PDEATHSIG`, so we
/// return the original (program, args) unchanged.
///
/// On macOS the `__supervise` wrapper watches the parent cuenv process
/// via `kqueue` and kills the child's process group on parent death.
fn wrap_with_supervisor(program: String, args: Vec<String>) -> (String, Vec<String>) {
    #[cfg(target_os = "macos")]
    {
        if let Ok(exe) = std::env::current_exe() {
            let mut new_args = Vec::with_capacity(args.len() + 2);
            new_args.push("__supervise".to_string());
            new_args.push(program);
            new_args.extend(args);
            return (exe.to_string_lossy().into_owned(), new_args);
        }
        // If we cannot locate our own executable, fall through to a
        // direct spawn — degrading gracefully is preferable to failing.
    }
    let _ = (cfg!(target_os = "macos"),); // touch cfg to silence unused-warning lint variants
    (program, args)
}

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
            emit_service_starting!(&self.name, self.command_display());
            self.log_state_update(StateUpdate {
                lifecycle: ServiceLifecycle::Starting,
                pid: None,
                exit_code: None,
                error: None,
            });

            let spawn_result = self.spawn_process().await;
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
                    let source = l.source.clone().unwrap_or("either".to_string());
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
                        self.stop_process(&mut child).await;
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
                        self.stop_process(&mut child).await;
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
                        self.stop_process(&mut child).await;
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

            // Wait for process exit, shutdown, or watch event
            let exit_status = tokio::select! {
                status = child.wait() => status,
                () = shutdown.cancelled() => {
                    emit_service_stopping!(&self.name);
                    self.stop_process(&mut child).await;
                    return SupervisorResult::Stopped;
                }
                Some(watch_event) = watch_rx.recv() => {
                    let changed: Vec<String> = watch_event.paths.iter()
                        .map(|p| p.to_string_lossy().into_owned())
                        .collect();
                    cuenv_events::emit_service_watch!(&self.name, &changed);
                    emit_service_restarting!(&self.name, "watch", attempt);
                    self.stop_process(&mut child).await;
                    restart_history.push_back(Instant::now());
                    attempt += 1;
                    continue;
                }
            };

            // Clean up output handles
            if let Some(h) = stdout_handle {
                h.abort();
            }
            if let Some(h) = stderr_handle {
                h.abort();
            }

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

    async fn spawn_process(&self) -> crate::Result<tokio::process::Child> {
        let (program, args) = self.resolve_command();

        let working_dir = self
            .service
            .dir
            .as_ref()
            .map(|d| self.project_root.join(d))
            .unwrap_or_else(|| self.project_root.clone());

        // On macOS, route the child through `cuenv __supervise` so it
        // gets killed if cuenv itself dies ungracefully. Linux uses
        // PR_SET_PDEATHSIG in pre_exec below and skips the wrapper.
        let (program, args) = wrap_with_supervisor(program, args);

        let mut cmd = Command::new(&program);
        cmd.args(&args)
            .current_dir(&working_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        // Set up process group on Unix so `cuenv stop <service>` can
        // target a single service's pgid. On Linux, also ask the kernel
        // to deliver SIGKILL to the child if cuenv itself dies for any
        // reason (including SIGKILL), so services cannot be orphaned.
        // macOS has no PR_SET_PDEATHSIG equivalent — we rely on a
        // dedicated `cuenv __supervise` wrapper there (see commands::supervise).
        #[cfg(unix)]
        {
            unsafe {
                cmd.pre_exec(|| {
                    libc::setpgid(0, 0);
                    #[cfg(target_os = "linux")]
                    {
                        // SAFETY: PR_SET_PDEATHSIG affects only the calling
                        // process; the value SIGKILL is a valid signal number
                        // and prctl does not retain the pointer argument.
                        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
                    }
                    Ok(())
                });
            }
        }

        // Resolve environment variables (including secrets) and apply to
        // the child process. Resolved secret values are registered with the
        // event system so they get redacted from service log output.
        let (resolved_env, secrets) =
            cuenv_core::environment::Environment::resolve_for_service_with_secrets(
                &self.name,
                &self.service.env,
            )
            .await?;
        if !secrets.is_empty() {
            cuenv_events::register_secrets(secrets.into_iter());
        }
        for (key, value) in resolved_env {
            cmd.env(key, value);
        }

        let child = cmd.spawn()?;
        Ok(child)
    }

    async fn stop_process(&self, child: &mut tokio::process::Child) {
        let shutdown_config = self.service.shutdown.as_ref();
        let signal = shutdown_config
            .and_then(|s| s.signal.as_deref())
            .unwrap_or("SIGTERM");
        let timeout = shutdown_config
            .and_then(|s| s.timeout.as_deref())
            .and_then(|t| parse_duration(t).ok())
            .unwrap_or(Duration::from_secs(10));

        if let Some(pid) = child.id() {
            #[cfg(unix)]
            {
                let sig = match signal {
                    "SIGINT" => libc::SIGINT,
                    "SIGHUP" => libc::SIGHUP,
                    "SIGQUIT" => libc::SIGQUIT,
                    _ => libc::SIGTERM,
                };
                unsafe {
                    libc::kill(-(pid as i32), sig);
                }
            }

            // Wait for graceful shutdown
            let wait_result = tokio::time::timeout(timeout, child.wait()).await;
            if wait_result.is_err() {
                // Force kill
                #[cfg(unix)]
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGKILL);
                }
                let _ = child.wait().await;
            }
        } else {
            let _ = child.kill().await;
        }

        emit_service_stopped!(
            &self.name,
            child.try_wait().ok().flatten().and_then(|s| s.code())
        );
    }

    fn resolve_command(&self) -> (String, Vec<String>) {
        use cuenv_core::manifest::Entrypoint;
        match &self.service.entrypoint {
            Entrypoint::Task(task) => {
                if let Some(ref script) = task.script {
                    let (cmd, flag) = task
                        .script_shell
                        .as_ref()
                        .map_or(("bash", "-c"), |s| s.command_and_flag());
                    (cmd.to_string(), vec![flag.to_string(), script.clone()])
                } else {
                    (task.command.clone(), task.args.to_vec())
                }
            }
            Entrypoint::Script(s) => {
                let (cmd, flag) = s
                    .script_shell
                    .as_ref()
                    .map_or(("bash", "-c"), |sh| sh.command_and_flag());
                (cmd.to_string(), vec![flag.to_string(), s.script.clone()])
            }
            Entrypoint::Command(c) => {
                let args: Vec<String> = c
                    .args
                    .iter()
                    .filter_map(|a| a.as_str().map(String::from))
                    .collect();
                (c.command.clone(), args)
            }
        }
    }

    fn command_display(&self) -> String {
        use cuenv_core::manifest::Entrypoint;
        match &self.service.entrypoint {
            Entrypoint::Task(task) => {
                if let Some(ref script) = task.script {
                    format!("script: {}", &script[..script.len().min(60)])
                } else if task.args.is_empty() {
                    task.command.clone()
                } else {
                    format!("{} {}", task.command, task.args.join(" "))
                }
            }
            Entrypoint::Script(s) => {
                format!("script: {}", &s.script[..s.script.len().min(60)])
            }
            Entrypoint::Command(c) => {
                let args: Vec<&str> = c.args.iter().filter_map(|a| a.as_str()).collect();
                if args.is_empty() {
                    c.command.clone()
                } else {
                    format!("{} {}", c.command, args.join(" "))
                }
            }
        }
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
        };
        self.session.update_service(&state)
    }
}
