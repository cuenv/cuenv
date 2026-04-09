//! External command readiness probe.
//!
//! Spawns a command and checks exit code 0 = ready.

use async_trait::async_trait;
use tokio::process::Command;

use super::{ProbeOutcome, ProbeRunner};

/// Probes readiness by running an external command.
pub struct CommandProbe {
    command: String,
    args: Vec<String>,
}

impl CommandProbe {
    /// Create a new command probe.
    #[must_use]
    pub fn new(command: String, args: Vec<String>) -> Self {
        Self { command, args }
    }
}

#[async_trait]
impl ProbeRunner for CommandProbe {
    async fn check(&self) -> ProbeOutcome {
        let result = Command::new(&self.command)
            .args(&self.args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .await;

        match result {
            Ok(status) if status.success() => ProbeOutcome::Ready,
            Ok(_) => ProbeOutcome::NotReady,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                ProbeOutcome::Fatal(format!("probe command not found: {}", self.command))
            }
            Err(_) => ProbeOutcome::NotReady,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_command_probe_ready() {
        let probe = CommandProbe::new("true".to_string(), vec![]);
        let result = probe.check().await;
        assert!(matches!(result, ProbeOutcome::Ready));
    }

    #[tokio::test]
    async fn test_command_probe_not_ready() {
        let probe = CommandProbe::new("false".to_string(), vec![]);
        let result = probe.check().await;
        assert!(matches!(result, ProbeOutcome::NotReady));
    }

    #[tokio::test]
    async fn test_command_probe_not_found() {
        let probe = CommandProbe::new("nonexistent_command_xyz".to_string(), vec![]);
        let result = probe.check().await;
        assert!(matches!(result, ProbeOutcome::Fatal(_)));
    }
}
