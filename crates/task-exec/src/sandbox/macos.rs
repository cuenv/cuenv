//! macOS `/usr/bin/sandbox-exec` prototype for strict hermetic execution.
//!
//! This is an explicitly approved prototype launcher exception: macOS runs
//! under a deny-by-default Seatbelt profile, with `publish_eligible = false`
//! until native macOS validation is completed.

use super::{StrictSandboxProof, compute_policy_digest};
use cuenv_core::{Error, Result};
use std::path::Path;
use tokio::process::Command;

const SANDBOX_EXEC_BIN: &str = "/usr/bin/sandbox-exec";

/// Probe whether `/usr/bin/sandbox-exec` is available on this macOS host.
pub(crate) fn probe(network_allowed: bool) -> std::result::Result<StrictSandboxProof, String> {
    if !Path::new(SANDBOX_EXEC_BIN).exists() {
        return Err(format!("{SANDBOX_EXEC_BIN} not found"));
    }

    let policy_digest = compute_policy_digest(&[
        ("platform", "macos-sandbox-exec".to_string()),
        (
            "network",
            if network_allowed {
                "allowed".to_string()
            } else {
                "disabled".to_string()
            },
        ),
    ]);

    Ok(StrictSandboxProof {
        platform: "macos-sandbox-exec",
        policy_digest,
        network_enforced: !network_allowed,
        // Publication remains gated on native validation of a deny-by-default profile.
        publish_eligible: false,
    })
}

/// Pre-prepared macOS Seatbelt profile plan.
#[derive(Clone)]
pub(crate) struct MacosSandboxPlan {
    profile: String,
}

impl MacosSandboxPlan {
    pub(crate) fn prepare(
        exec_root: &Path,
        _workdir: &Path,
        task_tmp: &Path,
        task_home: &Path,
        network_allowed: bool,
    ) -> Result<(Self, StrictSandboxProof)> {
        let proof = probe(network_allowed).map_err(Error::configuration)?;

        let exec_root_escaped = escape_seatbelt_path(exec_root);
        let tmp_escaped = escape_seatbelt_path(task_tmp);
        let home_escaped = escape_seatbelt_path(task_home);

        let network_rule = if network_allowed {
            "(allow network*)"
        } else {
            "(deny network*)"
        };

        let profile = format!(
            "(version 1)\n\
             (deny default)\n\
             (allow process*)\n\
             (allow sysctl-read)\n\
             (allow mach-lookup)\n\
             (allow file-read* (subpath \"/usr\") (subpath \"/bin\") (subpath \"/sbin\") (subpath \"/System\") (subpath \"/Library\") (subpath \"/dev\") (subpath \"/private/var/db\") (literal \"/\"))\n\
             (allow file-read* file-write* (subpath \"{exec_root_escaped}\") (subpath \"{tmp_escaped}\") (subpath \"{home_escaped}\"))\n\
             (allow file-write* (literal \"/dev/null\") (literal \"/dev/zero\"))\n\
             {network_rule}\n"
        );

        Ok((Self { profile }, proof))
    }

    pub(crate) fn build_command(&self, program: &str, args: &[String]) -> Result<Command> {
        let mut command = Command::new(SANDBOX_EXEC_BIN);
        command.arg("-p").arg(&self.profile).arg(program).args(args);
        Ok(command)
    }
}

fn escape_seatbelt_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}
