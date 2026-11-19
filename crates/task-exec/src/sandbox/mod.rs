//! OS-level strict sandboxing for hermetic tasks.
//!
//! [`Sandbox::Dir`] isolates relative workspace paths inside a per-action exec
//! root, but does not prevent a task from reading absolute host paths or using
//! the network. [`Sandbox::Strict`] and [`Sandbox::Auto`] add OS-level
//! confinement:
//!
//! - **Linux:** unprivileged user, mount, IPC, UTS, and (when `network: false`)
//!   network namespaces configured in an async-signal-safe `pre_exec` hook
//!   using pre-fork allocated C strings. Only explicit read-only runtime roots,
//!   minimal `/dev` nodes, the per-action exec root, and private `/tmp`/`HOME`
//!   directories are mounted.
//! - **macOS:** `/usr/bin/sandbox-exec` prototype using a deny-by-default
//!   Seatbelt profile. Because `sandbox-exec` is deprecated and not yet
//!   natively validated as a shared-cache security boundary, macOS runs set
//!   `publish_eligible = false`.

#[cfg(target_os = "linux")]
pub(crate) mod linux;
#[cfg(target_os = "macos")]
pub(crate) mod macos;

use cuenv_core::{Error, Result};
use cuenv_manifest::tasks::Sandbox;
use sha2::{Digest as _, Sha256};
use std::path::Path;
use tokio::process::Command;

/// Proof that a task was (or will be) confined by an OS-level sandbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrictSandboxProof {
    /// Platform implementation identifier (e.g. `"linux-namespaces"`, `"macos-sandbox-exec"`).
    pub platform: &'static str,
    /// Canonical SHA-256 hex digest of the sandbox policy and visible runtime
    /// roots' identity. Folded into `cuenv.sandbox.policy` on strict actions.
    pub policy_digest: String,
    /// Whether network isolation (`CLONE_NEWNET` or Seatbelt `(deny network*)`)
    /// is actually enforced for this run.
    pub network_enforced: bool,
    /// Whether this sandbox proof is strong enough to authorize publishing
    /// results to a shared remote cache.
    ///
    /// Currently `true` only on validated Linux namespace confinement with
    /// network isolation enforced.
    pub publish_eligible: bool,
}

/// The sandbox tier actually enforced for an execution attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EffectiveSandbox {
    /// No isolation: runs directly in the project checkout.
    None,
    /// Per-action exec root only (not an OS security boundary).
    Dir,
    /// OS-level strict confinement with a verified proof.
    Strict(StrictSandboxProof),
}

impl EffectiveSandbox {
    /// Whether this tier requires a per-action exec root.
    #[must_use]
    pub fn uses_exec_root(&self) -> bool {
        !matches!(self, Self::None)
    }

    /// The `cuenv.sandbox` platform property recorded in the action key.
    #[must_use]
    pub fn key_value(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Dir => "dir",
            Self::Strict(_) => "strict",
        }
    }

    /// Return the strict proof if OS-level confinement is active.
    #[must_use]
    pub fn strict_proof(&self) -> Option<&StrictSandboxProof> {
        match self {
            Self::Strict(proof) => Some(proof),
            Self::None | Self::Dir => None,
        }
    }
}

/// Result of resolving a task's requested [`Sandbox`] against host capabilities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxResolution {
    /// The tier that will actually be enforced.
    pub effective: EffectiveSandbox,
    /// Diagnostic reason when `Sandbox::Auto` degraded to `Sandbox::Dir`.
    pub fallback_reason: Option<String>,
}

/// Probe the host and resolve a task's requested [`Sandbox`] tier.
///
/// - [`Sandbox::None`] and [`Sandbox::Dir`] always resolve directly.
/// - [`Sandbox::Strict`] fails closed with a configuration error if the host
///   cannot enforce OS-level confinement — even when caching is disabled.
/// - [`Sandbox::Auto`] attempts OS-level confinement and falls back to
///   [`EffectiveSandbox::Dir`] with a diagnostic reason when unavailable.
///
/// # Errors
///
/// Returns an error only when `requested == Sandbox::Strict` and the host
/// cannot provide strict confinement.
pub fn resolve_sandbox(requested: Sandbox, network_allowed: bool) -> Result<SandboxResolution> {
    match requested {
        Sandbox::None => Ok(SandboxResolution {
            effective: EffectiveSandbox::None,
            fallback_reason: None,
        }),
        Sandbox::Dir => Ok(SandboxResolution {
            effective: EffectiveSandbox::Dir,
            fallback_reason: None,
        }),
        Sandbox::Strict => match probe_strict(network_allowed) {
            Ok(proof) => Ok(SandboxResolution {
                effective: EffectiveSandbox::Strict(proof),
                fallback_reason: None,
            }),
            Err(reason) => Err(Error::configuration(format!(
                "task requires hermetic.sandbox: \"strict\", but OS-level confinement is \
                 unavailable on this host: {reason}"
            ))),
        },
        Sandbox::Auto => match probe_strict(network_allowed) {
            Ok(proof) => Ok(SandboxResolution {
                effective: EffectiveSandbox::Strict(proof),
                fallback_reason: None,
            }),
            Err(reason) => Ok(SandboxResolution {
                effective: EffectiveSandbox::Dir,
                fallback_reason: Some(reason),
            }),
        },
    }
}

/// Pre-fork prepared strict sandbox plan ready to attach to a child [`Command`].
pub struct StrictSandboxPlan {
    proof: StrictSandboxProof,
    #[cfg(target_os = "linux")]
    inner: linux::LinuxSandboxPlan,
    #[cfg(target_os = "macos")]
    inner: macos::MacosSandboxPlan,
}

impl StrictSandboxPlan {
    /// Prepare a strict sandbox plan **before fork** so all heap allocations
    /// and path resolutions happen safely in the parent process.
    ///
    /// # Errors
    ///
    /// Returns an error if the platform does not support strict confinement or
    /// if the sandbox root staging directories cannot be created.
    pub fn prepare(
        exec_root: &Path,
        workdir: &Path,
        task_tmp: &Path,
        task_home: &Path,
        network_allowed: bool,
    ) -> Result<Self> {
        #[cfg(target_os = "linux")]
        {
            let (inner, proof) = linux::LinuxSandboxPlan::prepare(
                exec_root,
                workdir,
                task_tmp,
                task_home,
                network_allowed,
            )?;
            Ok(Self { proof, inner })
        }

        #[cfg(target_os = "macos")]
        {
            let (inner, proof) = macos::MacosSandboxPlan::prepare(
                exec_root,
                workdir,
                task_tmp,
                task_home,
                network_allowed,
            )?;
            Ok(Self { proof, inner })
        }

        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = (exec_root, workdir, task_tmp, task_home, network_allowed);
            Err(Error::configuration(format!(
                "strict OS sandboxing is not supported on {}",
                std::env::consts::OS
            )))
        }
    }

    /// Return the proof associated with this prepared sandbox plan.
    #[must_use]
    pub fn proof(&self) -> &StrictSandboxProof {
        &self.proof
    }

    /// Build a confined [`Command`] ready for spawning.
    ///
    /// On Linux this configures `Command::new(program)` with an
    /// async-signal-safe `pre_exec` hook. On macOS this wraps the program with
    /// `/usr/bin/sandbox-exec -p <profile>`.
    ///
    /// # Errors
    ///
    /// Returns an error if the platform command cannot be constructed.
    pub fn build_command(&self, program: &str, args: &[String]) -> Result<Command> {
        #[cfg(target_os = "linux")]
        {
            Ok(self.inner.build_command(program, args))
        }

        #[cfg(target_os = "macos")]
        {
            self.inner.build_command(program, args)
        }

        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = (program, args);
            Err(Error::configuration(format!(
                "strict OS sandboxing is not supported on {}",
                std::env::consts::OS
            )))
        }
    }
}

fn probe_strict(network_allowed: bool) -> std::result::Result<StrictSandboxProof, String> {
    #[cfg(target_os = "linux")]
    {
        linux::probe(network_allowed)
    }

    #[cfg(target_os = "macos")]
    {
        macos::probe(network_allowed)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = network_allowed;
        Err(format!(
            "strict OS sandboxing is not implemented on {}",
            std::env::consts::OS
        ))
    }
}

/// Compute a canonical SHA-256 digest over policy entries (`key=value` pairs).
pub(crate) fn compute_policy_digest(entries: &[(&str, String)]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"cuenv.sandbox.policy.v1\n");
    for (k, v) in entries {
        hasher.update(k.as_bytes());
        hasher.update(b"=");
        hasher.update(v.as_bytes());
        hasher.update(b"\n");
    }
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_and_none_resolve_without_probing_strict() {
        let dir = resolve_sandbox(Sandbox::Dir, false).unwrap();
        assert_eq!(dir.effective, EffectiveSandbox::Dir);
        assert!(dir.fallback_reason.is_none());

        let none = resolve_sandbox(Sandbox::None, false).unwrap();
        assert_eq!(none.effective, EffectiveSandbox::None);
        assert!(none.fallback_reason.is_none());
    }

    #[test]
    fn policy_digest_changes_when_network_or_roots_change() {
        let a =
            compute_policy_digest(&[("platform", "linux".into()), ("network", "disabled".into())]);
        let b =
            compute_policy_digest(&[("platform", "linux".into()), ("network", "allowed".into())]);
        assert_ne!(a, b);
    }
}
