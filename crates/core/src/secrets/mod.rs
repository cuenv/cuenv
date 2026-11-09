//! Secret and resolver types
//!
//! Based on schema/secrets.cue
//!
//! This module provides:
//! - `Secret`: CUE-compatible secret definition with resolver-based resolution
//! - `SecretRegistry`: Dynamic resolver registration and lookup
//! - Re-exports from `cuenv_secrets`: Trait-based secret resolution system
//!
//! Provider-backed resolvers (1Password, AWS, GCP, Infisical) are no longer
//! wired here: the CLI composition root installs them into the process-wide
//! registry (`cuenv_secrets::install_registry_factory`), and resolution
//! falls back to the dependency-free built-ins (`env` + `exec`) when no
//! factory is installed (RFC-0006 phase 3b).

pub mod error;
mod secret_ext;

pub use cuenv_secrets::resolvers::{EnvSecretResolver, ExecSecretResolver};
pub use cuenv_secrets::{
    BatchResolver, ResolvedSecrets, SaltConfig, SecretError, SecretRegistry, SecretResolver,
    SecretSpec, compute_secret_fingerprint, global_registry, install_registry_factory,
};
pub use error::SecretResolutionError;
pub use secret_ext::{ExecResolver, SecretExt};

pub(crate) use cuenv_manifest::secrets::Secret;
