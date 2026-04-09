//! Log pattern readiness probe.
//!
//! Watches service output for a regex match to determine readiness.
//! This probe is event-driven rather than polling-based.

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;

use super::{ProbeOutcome, ProbeRunner};

/// Probes readiness by watching service output for a regex pattern match.
pub struct LogProbe {
    pattern: regex::Regex,
    matched: Arc<Mutex<bool>>,
    source: LogSource,
}

/// Which output stream to watch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogSource {
    /// Watch stdout only.
    Stdout,
    /// Watch stderr only.
    Stderr,
    /// Watch both streams.
    Either,
}

impl LogProbe {
    /// Create a new log probe.
    ///
    /// # Errors
    ///
    /// Returns an error if the regex pattern is invalid.
    pub fn new(pattern: &str, source: String) -> crate::Result<Self> {
        let re = regex::Regex::new(pattern).map_err(|e| crate::Error::ProbeFailed {
            name: String::new(),
            message: format!("invalid regex pattern: {e}"),
        })?;

        let source = match source.as_str() {
            "stdout" => LogSource::Stdout,
            "stderr" => LogSource::Stderr,
            _ => LogSource::Either,
        };

        Ok(Self {
            pattern: re,
            matched: Arc::new(Mutex::new(false)),
            source,
        })
    }

    /// Feed a line of output to the probe. Call this from the supervisor
    /// output handler. Returns `true` if the line matches.
    pub async fn feed_line(&self, line: &str, stream: &str) -> bool {
        let should_check = match self.source {
            LogSource::Stdout => stream == "stdout",
            LogSource::Stderr => stream == "stderr",
            LogSource::Either => true,
        };

        if should_check && self.pattern.is_match(line) {
            *self.matched.lock().await = true;
            return true;
        }

        false
    }

    /// Get a clone of the matched flag for sharing with the probe runner.
    #[must_use]
    pub fn matched_flag(&self) -> Arc<Mutex<bool>> {
        Arc::clone(&self.matched)
    }
}

#[async_trait]
impl ProbeRunner for LogProbe {
    async fn check(&self) -> ProbeOutcome {
        if *self.matched.lock().await {
            ProbeOutcome::Ready
        } else {
            ProbeOutcome::NotReady
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_log_probe_not_ready() {
        let probe = LogProbe::new("listening on", "either".to_string()).unwrap();
        assert!(matches!(probe.check().await, ProbeOutcome::NotReady));
    }

    #[tokio::test]
    async fn test_log_probe_ready_on_match() {
        let probe = LogProbe::new("listening on .*:8080", "either".to_string()).unwrap();
        probe.feed_line("server listening on 0.0.0.0:8080", "stdout").await;
        assert!(matches!(probe.check().await, ProbeOutcome::Ready));
    }

    #[tokio::test]
    async fn test_log_probe_source_filter() {
        let probe = LogProbe::new("ready", "stdout".to_string()).unwrap();
        // Feed on stderr — should not match
        probe.feed_line("ready", "stderr").await;
        assert!(matches!(probe.check().await, ProbeOutcome::NotReady));
        // Feed on stdout — should match
        probe.feed_line("ready", "stdout").await;
        assert!(matches!(probe.check().await, ProbeOutcome::Ready));
    }

    #[test]
    fn test_invalid_regex() {
        let result = LogProbe::new("[invalid", "either".to_string());
        assert!(result.is_err());
    }
}
