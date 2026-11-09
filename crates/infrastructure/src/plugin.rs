//! Launching Terraform provider binaries and speaking their gRPC protocol.
//!
//! Providers are HashiCorp `go-plugin` servers. The host starts the binary
//! with a magic cookie in the environment; the plugin replies with a single
//! handshake line on standard output describing where it listens:
//!
//! ```text
//! CORE-PROTOCOL-VERSION|APP-PROTOCOL-VERSION|NETWORK|ADDRESS|PROTOCOL|SERVER-CERT
//! 1|6|unix|/tmp/cuenv-plugin-1a2b/plugin123|grpc|
//! ```
//!
//! cuenv offers protocol versions 5 and 6 and speaks whichever the provider
//! selects. It does not request AutoMTLS, so providers serve plaintext gRPC
//! on a private unix socket, the same as Terraform with
//! `TF_DISABLE_PLUGIN_TLS`.
//!
//! Each provider gets a private, short socket directory (unix socket paths
//! are limited to about 108 bytes) that is removed when the provider stops
//! (or by [`crate::Cancellation::terminate_providers`] on a forced exit),
//! and never inherits the state store's credentials.
//!
//! Providers run in their own process group, so an interrupt typed at the
//! terminal reaches cuenv only; cuenv then asks them to stop or kills them
//! through [`crate::Cancellation`]. Killing a provider kills its whole
//! process group, so processes it started die with it. They are also
//! killed whenever their client is dropped.
//!
//! On Linux each provider also asks the kernel to kill it when its parent
//! goes away (`PR_SET_PDEATHSIG`), so even a cuenv killed with `SIGKILL`
//! leaves no provider behind. The kernel sends that signal when the
//! *thread* that spawned the provider exits, so providers must be launched
//! from a long-lived async runtime thread (as [`ProviderClient::launch`]
//! is, being async), never from `spawn_blocking` or `block_in_place`,
//! whose threads come and go. macOS has no equivalent; there a provider
//! outlives a cuenv killed with `SIGKILL`.
//!
//! Tracing records provider log metadata only. Serious lines have control
//! characters removed and reach error reports through the redacted event path.

mod provider_client;
mod write_only_redaction;

pub(crate) use provider_client::redact_provider_text;
pub use provider_client::{
    ApplyRequest, Handshake, ISOLATED_INHERITED_ENVIRONMENT_VARIABLES, LaunchOptions, LogRedactor,
    PlanRequest, Protocol, ProviderClient, ProviderProcess, ReadRequest, install_log_redactor,
    isolated_withheld_names,
};
