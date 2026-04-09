//! Per-service supervisor.
//!
//! Manages the spawn-probe-restart-shutdown lifecycle for a single service.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;
use tracing::debug;

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

/// Result of supervisor execution.
#[derive(Debug)]
pub enum SupervisorResult {
    /// Service stopped normally (cuenv down / Ctrl-C).
    Stopped,
    /// Service failed and exhausted restart attempts.
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

/// Supervisor for a single service.
pub struct ServiceSupervisor {
    name: String,
    service: Service,
    project_root: PathBuf,
    session: Arc<SessionManager>,
}

impl ServiceSupervisor {
    /// Create a new supervisor.
    #[must_use]
    pub fn new(
        name: String,
        service: Service,
        project_root: PathBuf,
        session: Arc<SessionManager>,
    ) -> Self {
        Self {
            name,
            service,
            project_root,
            session,
        }
    }

    /// Run the supervisor loop.
    ///
    /// Spawns the service process, runs readiness probes, handles restarts,
    /// and responds to file watcher events. Returns when the service reaches
    /// a terminal state or the shutdown token is cancelled.
    pub async fn run(
        &self,
        shutdown: CancellationToken,
        ready_notify: Arc<Notify>,
    ) -> SupervisorResult {
        let restart_policy = self.service.restart.as_ref();
        let mode = restart_policy
            .and_then(|r| r.mode.as_deref())
            .unwrap_or("onFailure");
        let max_restarts = restart_policy
            .and_then(|r| r.max_restarts)
            .unwrap_or(5);
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
        let mut _ever_ready = false;

        // Set up file watcher if configured
        let (watch_tx, mut watch_rx) = mpsc::channel::<WatchEvent>(16);
        let _watcher = self.service.watch.as_ref().and_then(|w| {
            let debounce = w
                .debounce
                .as_deref()
                .and_then(|d| parse_duration(d).ok())
                .unwrap_or(Duration::from_millis(200));
            let ignore = w.ignore.clone().unwrap_or_default();
            ServiceWatcher::start(
                &self.project_root,
                &w.paths,
                &ignore,
                debounce,
                watch_tx,
            )
            .ok()
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
                self.update_state(ServiceLifecycle::Failed, None, None, Some(&msg))
                    .ok();
                return SupervisorResult::Failed(msg);
            }

            // Spawn the service process
            emit_service_starting!(&self.name, self.command_display());
            self.update_state(ServiceLifecycle::Starting, None, None, None)
                .ok();

            let spawn_result = self.spawn_process().await;
            let mut child = match spawn_result {
                Ok(c) => c,
                Err(e) => {
                    let msg = format!("failed to spawn: {e}");
                    emit_service_failed!(&self.name, &msg);
                    self.update_state(ServiceLifecycle::Failed, None, None, Some(&msg))
                        .ok();
                    return SupervisorResult::Failed(msg);
                }
            };

            let pid = child.id();
            self.update_state(ServiceLifecycle::Starting, pid, None, None)
                .ok();

            // Set up output streaming
            let stdout = child.stdout.take();
            let stderr = child.stderr.take();

            // Create log probe if needed
            let log_probe = self
                .service
                .readiness
                .as_ref()
                .and_then(|r| match r {
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

            // Run readiness probe
            let ready = if let Some(ref readiness) = self.service.readiness {
                match probes::create_probe(readiness) {
                    Ok((probe, config)) => {
                        let result = probes::run_probe_loop(probe.as_ref(), &config).await;
                        match result {
                            ProbeLoopResult::Ready { after_ms } => {
                                emit_service_ready!(&self.name, after_ms);
                                true
                            }
                            ProbeLoopResult::TimedOut { after_ms } => {
                                emit_service_ready_timeout!(&self.name, after_ms);
                                false
                            }
                            ProbeLoopResult::Fatal(msg) => {
                                emit_service_failed!(&self.name, &msg);
                                false
                            }
                        }
                    }
                    Err(e) => {
                        emit_service_failed!(&self.name, e.to_string());
                        false
                    }
                }
            } else {
                // No readiness probe — consider immediately ready
                emit_service_ready!(&self.name, 0_u64);
                true
            };

            if ready {
                _ever_ready = true;
                backoff.reset();
                self.update_state(ServiceLifecycle::Ready, pid, None, None)
                    .ok();
                ready_notify.notify_one();
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

            let exit_code = exit_status
                .ok()
                .and_then(|s| s.code());
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
                self.update_state(ServiceLifecycle::Restarting, None, None, None)
                    .ok();

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
                    self.update_state(ServiceLifecycle::Failed, None, exit_code, Some(&msg))
                        .ok();
                    return SupervisorResult::Failed(msg);
                }
                self.update_state(ServiceLifecycle::Stopped, None, exit_code, None)
                    .ok();
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

        let mut cmd = Command::new(&program);
        cmd.args(&args)
            .current_dir(&working_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        // Set up process group on Unix
        #[cfg(unix)]
        {
            unsafe {
                cmd.pre_exec(|| {
                    libc::setpgid(0, 0);
                    Ok(())
                });
            }
        }

        // Apply environment variables
        for (key, value) in &self.service.env {
            if let Some(s) = value.as_str() {
                cmd.env(key, s);
            }
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

        emit_service_stopped!(&self.name, child.try_wait().ok().flatten().and_then(|s| s.code()));
    }

    fn resolve_command(&self) -> (String, Vec<String>) {
        if let Some(ref script) = self.service.script {
            let (cmd, flag) = self
                .service
                .script_shell
                .as_ref()
                .map_or(("bash", "-c"), |s| s.command_and_flag());
            (cmd.to_string(), vec![flag.to_string(), script.clone()])
        } else {
            let command = self
                .service
                .command
                .clone()
                .unwrap_or_default();
            let args: Vec<String> = self
                .service
                .args
                .iter()
                .filter_map(|a| a.as_str().map(String::from))
                .collect();
            (command, args)
        }
    }

    fn command_display(&self) -> String {
        if let Some(ref script) = self.service.script {
            format!("script: {}", &script[..script.len().min(60)])
        } else {
            let cmd = self.service.command.as_deref().unwrap_or("(none)");
            let args: Vec<&str> = self
                .service
                .args
                .iter()
                .filter_map(|a| a.as_str())
                .collect();
            if args.is_empty() {
                cmd.to_string()
            } else {
                format!("{} {}", cmd, args.join(" "))
            }
        }
    }

    fn update_state(
        &self,
        lifecycle: ServiceLifecycle,
        pid: Option<u32>,
        exit_code: Option<i32>,
        error: Option<&str>,
    ) -> crate::Result<()> {
        // Try to read existing state to preserve accumulated fields
        let existing = self.session.read_service(&self.name).ok();

        let state = ServiceState {
            name: self.name.clone(),
            lifecycle,
            pid,
            started_at: existing
                .as_ref()
                .and_then(|s| s.started_at)
                .or(Some(chrono::Utc::now())),
            ready_at: if lifecycle == ServiceLifecycle::Ready {
                Some(chrono::Utc::now())
            } else {
                existing.as_ref().and_then(|s| s.ready_at)
            },
            restarts: existing.as_ref().map_or(0, |s| {
                if lifecycle == ServiceLifecycle::Restarting {
                    s.restarts + 1
                } else {
                    s.restarts
                }
            }),
            exit_code,
            error: error.map(String::from),
        };
        self.session.update_service(&state)
    }
}
