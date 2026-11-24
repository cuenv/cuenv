//! OS-level strict sandboxing for hermetic tasks.
//!
//! [`Sandbox::Dir`] isolates relative workspace paths inside a per-action exec
//! root, but does not prevent a task from reading absolute host paths or using
//! the network. [`Sandbox::Strict`] and [`Sandbox::Auto`] add OS-level
//! confinement:
//!
//! - **Linux:** unprivileged user, PID, mount, IPC, UTS, and optional network
//!   namespaces. The launcher waits for PID namespace teardown and exposes
//!   only explicit read-only runtime roots, minimal `/dev` nodes, the per-action
//!   exec root, and private `/tmp` and `HOME`. Shared publication requires a
//!   validated immutable runtime image.
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
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::process::Stdio;
use tokio::process::Command;

/// Pinned Nix runtime context for Linux strict sandboxing without a
/// pre-mounted SquashFS/EROFS `CUENV_SANDBOX_RUNTIME_ROOT`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NixSandboxContext {
    /// Locked Nix runtime digest from `cuenv.lock` (`runtime.nix.digest`).
    pub lock_digest: String,
    /// Root `/nix/store/<hash>-<name>` paths referenced by the task environment.
    pub store_roots: Vec<PathBuf>,
    /// `PATH` entries under `/nix/store` from the task environment, used to
    /// locate `/bin/sh` and `/usr/bin/env` inside the Nix closure.
    pub path_entries: Vec<PathBuf>,
}

/// Extract a [`NixSandboxContext`] from a project's locked runtime identity
/// properties and the task's execution environment.
///
/// Returns `None` when the project does not use a locked Nix runtime or when
/// no `/nix/store/<hash>-<name>` paths appear in the environment.
#[must_use]
pub fn extract_nix_sandbox_context<'a>(
    runtime_properties: &BTreeMap<String, String>,
    env_vars: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Option<NixSandboxContext> {
    if runtime_properties.get("runtime.kind").map(String::as_str) != Some("nix") {
        return None;
    }
    let lock_digest = runtime_properties
        .get("runtime.nix.digest")
        .filter(|digest| !digest.is_empty())?
        .clone();

    let mut store_roots = BTreeSet::new();
    let mut path_entries = Vec::new();
    for (key, value) in env_vars {
        collect_nix_store_roots(value, &mut store_roots);
        if key == "PATH" {
            for entry in std::env::split_paths(value) {
                if entry.starts_with("/nix/store/") && !path_entries.contains(&entry) {
                    path_entries.push(entry);
                }
            }
        }
    }

    if store_roots.is_empty() {
        return None;
    }

    Some(NixSandboxContext {
        lock_digest,
        store_roots: store_roots.into_iter().collect(),
        path_entries,
    })
}

/// Scan `value` for `/nix/store/<32-nixbase32>-<name>` root store paths.
fn collect_nix_store_roots(value: &str, roots: &mut BTreeSet<PathBuf>) {
    const PREFIX: &str = "/nix/store/";
    let mut rest = value;
    while let Some(idx) = rest.find(PREFIX) {
        let after_prefix = &rest[idx + PREFIX.len()..];
        let end = after_prefix
            .find(|ch: char| !is_nix_store_name_char(ch))
            .unwrap_or(after_prefix.len());
        let candidate = &after_prefix[..end];
        if is_valid_nix_store_basename(candidate) {
            roots.insert(PathBuf::from(format!("{PREFIX}{candidate}")));
        }
        rest = &after_prefix[end..];
    }
}

pub(crate) fn is_valid_nix_store_basename(name: &str) -> bool {
    let bytes = name.as_bytes();
    // Nix store paths are `<32 nixbase32 chars>-<name>` (at least 34 bytes).
    if bytes.len() < 34 || bytes[32] != b'-' {
        return false;
    }
    bytes[..32].iter().all(|b| is_nixbase32(*b))
        && bytes[33..]
            .iter()
            .all(|b| is_nix_store_name_char(char::from(*b)))
}

const fn is_nixbase32(byte: u8) -> bool {
    matches!(
        byte,
        b'0'..=b'9' | b'a'..=b'd' | b'f'..=b'n' | b'p'..=b's' | b'v'..=b'z'
    )
}

const fn is_nix_store_name_char(ch: char) -> bool {
    ch.is_ascii_alphanumeric() || matches!(ch, '+' | '-' | '.' | '_' | '?' | '=')
}

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
    /// Currently `true` on Linux with network isolation and either a verified
    /// read-only runtime image or a verified pinned Nix store closure.
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

/// Network access requested for a task sandbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkPolicy {
    /// Keep the host network.
    Allowed,
    /// Run with network access disabled.
    Disabled,
}

impl NetworkPolicy {
    pub(crate) fn for_task(task: &crate::Task) -> Self {
        if task.network_allowed() {
            Self::Allowed
        } else {
            Self::Disabled
        }
    }
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
pub fn resolve_sandbox(requested: Sandbox, network: NetworkPolicy) -> Result<SandboxResolution> {
    resolve_sandbox_with_nix(requested, network, None)
}

/// Probe the host and resolve a task's requested [`Sandbox`] tier, optionally
/// using a pinned Nix closure runtime when `CUENV_SANDBOX_RUNTIME_ROOT` is unset.
///
/// # Errors
///
/// Returns an error only when `requested == Sandbox::Strict` and the host
/// cannot provide strict confinement.
pub fn resolve_sandbox_with_nix(
    requested: Sandbox,
    network: NetworkPolicy,
    nix_context: Option<&NixSandboxContext>,
) -> Result<SandboxResolution> {
    match requested {
        Sandbox::None => Ok(SandboxResolution {
            effective: EffectiveSandbox::None,
            fallback_reason: None,
        }),
        Sandbox::Dir => Ok(SandboxResolution {
            effective: EffectiveSandbox::Dir,
            fallback_reason: None,
        }),
        Sandbox::Strict => match probe_strict(network, nix_context) {
            Ok(proof) => Ok(SandboxResolution {
                effective: EffectiveSandbox::Strict(proof),
                fallback_reason: None,
            }),
            Err(reason) => Err(Error::configuration(format!(
                "task requires hermetic.sandbox: \"strict\", but OS-level confinement is \
                 unavailable on this host: {reason}"
            ))),
        },
        Sandbox::Auto => match probe_strict(network, nix_context) {
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

/// Paths and policy for a strict sandbox.
#[derive(Clone, Copy)]
pub struct StrictSandboxOptions<'paths> {
    /// Execution root containing staged inputs.
    pub exec_root: &'paths Path,
    /// Start directory inside the execution root.
    pub workdir: &'paths Path,
    /// Staged scratch space.
    pub task_tmp: &'paths Path,
    /// Staged task home.
    pub task_home: &'paths Path,
    /// Network access.
    pub network: NetworkPolicy,
    /// Optional pinned Nix runtime context for Linux strict sandboxing.
    pub nix_context: Option<&'paths NixSandboxContext>,
}

pub(crate) type SandboxExchange =
    std::pin::Pin<Box<dyn Future<Output = std::io::Result<()>> + Send>>;

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
    pub fn prepare(options: StrictSandboxOptions<'_>) -> Result<Self> {
        #[cfg(target_os = "linux")]
        {
            let (inner, proof) = linux::LinuxSandboxPlan::prepare(options)?;
            Ok(Self { proof, inner })
        }

        #[cfg(target_os = "macos")]
        {
            let (inner, proof) = macos::MacosSandboxPlan::prepare(options)?;
            Ok(Self { proof, inner })
        }

        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = options;
            Err(Error::configuration(format!(
                "strict OS sandboxing is not supported on {}",
                std::env::consts::OS
            )))
        }
    }

    /// Transfer the finished command to the CLI launcher without exposing
    /// task-controlled data to the dynamic loader or parent environment.
    #[cfg(target_os = "linux")]
    pub(crate) fn finish_command(
        &self,
        command: Command,
    ) -> Result<(Command, Option<Stdio>, Option<SandboxExchange>)> {
        linux::launcher::finish(&self.inner, command)
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
            let mut command = self.inner.build_command(program, args);
            command.kill_on_drop(true);
            Ok(command)
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

/// Dispatch an internal sandbox invocation before reading the task environment
/// or applying CLI process hardening. Returns only for a regular invocation.
#[cfg(target_os = "linux")]
pub fn dispatch_linux_launcher() {
    linux::launcher::dispatch();
}

/// Register the trusted cuenv executable for subsequent sandbox launches.
#[cfg(target_os = "linux")]
pub fn register_linux_launcher() {
    linux::launcher::register();
}

fn probe_strict(
    network: NetworkPolicy,
    nix_context: Option<&NixSandboxContext>,
) -> std::result::Result<StrictSandboxProof, String> {
    #[cfg(target_os = "linux")]
    {
        linux::probe(network, nix_context)
    }

    #[cfg(target_os = "macos")]
    {
        let _ = nix_context;
        macos::probe(network)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (network, nix_context);
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
        let dir = resolve_sandbox(Sandbox::Dir, NetworkPolicy::Disabled).unwrap();
        assert_eq!(dir.effective, EffectiveSandbox::Dir);
        assert!(dir.fallback_reason.is_none());

        let none = resolve_sandbox(Sandbox::None, NetworkPolicy::Disabled).unwrap();
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

    #[test]
    fn extract_nix_sandbox_context_collects_valid_store_roots_and_path_entries() {
        let mut props = BTreeMap::new();
        props.insert("runtime.kind".to_string(), "nix".to_string());
        props.insert(
            "runtime.nix.digest".to_string(),
            "sha256:abc123".to_string(),
        );

        let bash = "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-bash-5.2";
        let coreutils = "/nix/store/1123456789abcdfghijklmnpqrsvwxyz-coreutils-9.5";
        let cacert = "/nix/store/2123456789abcdfghijklmnpqrsvwxyz-nss-cacert-3.98";

        let path = format!("{bash}/bin:{coreutils}/bin:/usr/bin:/bin");
        let cert = format!("{cacert}/etc/ssl/certs/ca-bundle.crt");
        let ctx = extract_nix_sandbox_context(
            &props,
            [
                ("PATH", path.as_str()),
                ("SSL_CERT_FILE", cert.as_str()),
                ("BAD", "/nix/store/not-a-valid-hash"),
            ],
        )
        .unwrap();

        assert_eq!(ctx.lock_digest, "sha256:abc123");
        assert_eq!(
            ctx.store_roots,
            vec![
                PathBuf::from(bash),
                PathBuf::from(coreutils),
                PathBuf::from(cacert),
            ]
        );
        assert_eq!(
            ctx.path_entries,
            vec![
                PathBuf::from(format!("{bash}/bin")),
                PathBuf::from(format!("{coreutils}/bin")),
            ]
        );
    }

    #[test]
    fn extract_nix_sandbox_context_returns_none_without_locked_nix_runtime() {
        let props = BTreeMap::new();
        assert!(
            extract_nix_sandbox_context(
                &props,
                [(
                    "PATH",
                    "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-bash-5.2/bin"
                )]
            )
            .is_none()
        );
    }
}
