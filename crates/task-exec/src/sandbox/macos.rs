//! macOS `/usr/bin/sandbox-exec` prototype for strict hermetic execution.
//!
//! This is an explicitly approved prototype launcher exception: macOS runs
//! under a deny-by-default Seatbelt profile, with `publish_eligible = false`
//! until native macOS validation is completed.

use super::{NetworkPolicy, StrictSandboxOptions, StrictSandboxProof, compute_policy_digest};
use cuenv_core::{Error, Result};
use std::path::Path;
use std::sync::OnceLock;
use tokio::process::Command;

const SANDBOX_EXEC_BIN: &str = "/usr/bin/sandbox-exec";

/// Probe whether `/usr/bin/sandbox-exec` can actually apply a Seatbelt profile
/// on this macOS host (e.g. nested sandboxes such as Nix Darwin builds deny
/// `sandbox_apply` with `EPERM`).
fn probe_seatbelt_available() -> std::result::Result<(), String> {
    static RESULT: OnceLock<std::result::Result<(), String>> = OnceLock::new();
    RESULT
        .get_or_init(|| {
            if !Path::new(SANDBOX_EXEC_BIN).exists() {
                return Err(format!("{SANDBOX_EXEC_BIN} not found"));
            }
            let output = std::process::Command::new(SANDBOX_EXEC_BIN)
                .arg("-p")
                .arg("(version 1)(allow default)")
                .arg("/usr/bin/true")
                .output()
                .map_err(|error| format!("failed to probe {SANDBOX_EXEC_BIN}: {error}"))?;
            if output.status.success() {
                Ok(())
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let detail = stderr.trim();
                if detail.is_empty() {
                    Err(format!(
                        "{SANDBOX_EXEC_BIN} probe exited with {}",
                        output.status
                    ))
                } else {
                    Err(format!("{SANDBOX_EXEC_BIN} unavailable: {detail}"))
                }
            }
        })
        .clone()
}

/// Probe whether `/usr/bin/sandbox-exec` is available on this macOS host.
pub(super) fn probe(network: NetworkPolicy) -> std::result::Result<StrictSandboxProof, String> {
    let network_allowed = network == NetworkPolicy::Allowed;
    probe_seatbelt_available()?;

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
pub(super) struct MacosSandboxPlan {
    profile: String,
}

impl MacosSandboxPlan {
    pub(super) fn prepare(options: StrictSandboxOptions<'_>) -> Result<(Self, StrictSandboxProof)> {
        let StrictSandboxOptions {
            exec_root,
            workdir: _,
            task_tmp,
            task_home,
            network,
            nix_context: _,
        } = options;
        let network_allowed = network == NetworkPolicy::Allowed;
        let proof = probe(network).map_err(Error::configuration)?;

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

    pub(super) fn build_command(&self, program: &str, args: &[String]) -> Command {
        let mut command = crate::process::task_command(SANDBOX_EXEC_BIN);
        command.arg("-p").arg(&self.profile).arg(program).args(args);
        command
    }
}

fn escape_seatbelt_path(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}
