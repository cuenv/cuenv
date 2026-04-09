//! Readiness probe system for services.
//!
//! Probes determine when a service is ready to accept traffic.
//! Each probe type implements the [`ProbeRunner`] trait.

pub mod command;
pub mod delay;
pub mod http;
pub mod log;
pub mod port;

use std::time::{Duration, Instant};

use async_trait::async_trait;

use crate::duration::{parse_duration, parse_duration_or};

/// Outcome of a single probe check.
#[derive(Debug, Clone)]
pub enum ProbeOutcome {
    /// Service is ready.
    Ready,
    /// Service is not yet ready (retry).
    NotReady,
    /// Unrecoverable probe error — stop probing.
    Fatal(String),
}

/// Trait for readiness probe implementations.
#[async_trait]
pub trait ProbeRunner: Send + Sync {
    /// Perform a single probe check.
    async fn check(&self) -> ProbeOutcome;
}

/// Configuration for the probe polling loop.
pub struct ProbeLoopConfig {
    /// Time between probe attempts.
    pub interval: Duration,
    /// Maximum time to wait for readiness.
    pub timeout: Duration,
    /// Initial delay before first probe.
    pub initial_delay: Duration,
}

/// Result of running the probe loop.
#[derive(Debug)]
pub enum ProbeLoopResult {
    /// Service became ready within timeout.
    Ready {
        /// Milliseconds from start to ready.
        after_ms: u64,
    },
    /// Readiness timed out.
    TimedOut {
        /// Milliseconds elapsed before timeout.
        after_ms: u64,
    },
    /// Fatal probe error.
    Fatal(String),
}

/// Run a probe in a polling loop with the given configuration.
pub async fn run_probe_loop(
    probe: &dyn ProbeRunner,
    config: &ProbeLoopConfig,
) -> ProbeLoopResult {
    let start = Instant::now();

    // Initial delay
    if !config.initial_delay.is_zero() {
        tokio::time::sleep(config.initial_delay).await;
    }

    loop {
        let elapsed = start.elapsed();
        if elapsed >= config.timeout {
            return ProbeLoopResult::TimedOut {
                after_ms: elapsed.as_millis() as u64,
            };
        }

        match probe.check().await {
            ProbeOutcome::Ready => {
                return ProbeLoopResult::Ready {
                    after_ms: elapsed.as_millis() as u64,
                };
            }
            ProbeOutcome::NotReady => {
                tokio::time::sleep(config.interval).await;
            }
            ProbeOutcome::Fatal(msg) => {
                return ProbeLoopResult::Fatal(msg);
            }
        }
    }
}

/// Build a `ProbeLoopConfig` from readiness common fields.
///
/// # Errors
///
/// Returns an error if duration strings are invalid.
pub fn build_probe_config(
    interval: Option<&str>,
    timeout: Option<&str>,
    initial_delay: Option<&str>,
) -> crate::Result<ProbeLoopConfig> {
    Ok(ProbeLoopConfig {
        interval: parse_duration_or(interval, Duration::from_millis(500))?,
        timeout: parse_duration_or(timeout, Duration::from_secs(60))?,
        initial_delay: parse_duration_or(initial_delay, Duration::ZERO)?,
    })
}

/// Create a probe runner and config from a service's readiness configuration.
///
/// # Errors
///
/// Returns an error if the readiness config is invalid.
pub fn create_probe(
    readiness: &cuenv_core::manifest::Readiness,
) -> crate::Result<(Box<dyn ProbeRunner>, ProbeLoopConfig)> {
    match readiness {
        cuenv_core::manifest::Readiness::Port(p) => {
            let config = build_probe_config(
                p.common.interval.as_deref(),
                p.common.timeout.as_deref(),
                p.common.initial_delay.as_deref(),
            )?;
            let host = p.host.clone().unwrap_or_else(|| "127.0.0.1".to_string());
            let runner = port::PortProbe::new(host, p.port);
            Ok((Box::new(runner), config))
        }
        cuenv_core::manifest::Readiness::Http(h) => {
            let config = build_probe_config(
                h.common.interval.as_deref(),
                h.common.timeout.as_deref(),
                h.common.initial_delay.as_deref(),
            )?;
            let expected = h.expect_status.clone().unwrap_or_else(|| vec![200, 201, 202, 203, 204, 205, 206]);
            let method = h.method.clone().unwrap_or_else(|| "GET".to_string());
            let runner = http::HttpProbe::new(h.url.clone(), expected, method);
            Ok((Box::new(runner), config))
        }
        cuenv_core::manifest::Readiness::Log(l) => {
            let config = build_probe_config(
                l.common.interval.as_deref(),
                l.common.timeout.as_deref(),
                l.common.initial_delay.as_deref(),
            )?;
            let source = l.source.clone().unwrap_or_else(|| "either".to_string());
            let runner = log::LogProbe::new(&l.pattern, source)?;
            Ok((Box::new(runner), config))
        }
        cuenv_core::manifest::Readiness::Command(c) => {
            let config = build_probe_config(
                c.common.interval.as_deref(),
                c.common.timeout.as_deref(),
                c.common.initial_delay.as_deref(),
            )?;
            let runner = command::CommandProbe::new(c.command.clone(), c.args.clone());
            Ok((Box::new(runner), config))
        }
        cuenv_core::manifest::Readiness::Delay(d) => {
            let delay = parse_duration(&d.delay)?;
            let config = ProbeLoopConfig {
                interval: Duration::from_millis(100),
                timeout: delay + Duration::from_secs(1),
                initial_delay: Duration::ZERO,
            };
            let runner = delay::DelayProbe::new(delay);
            Ok((Box::new(runner), config))
        }
    }
}
