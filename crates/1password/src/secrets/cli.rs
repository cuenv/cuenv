use cuenv_secrets::{ResolveSecret, SecretError};
use std::collections::{BTreeSet, HashMap};
use tokio::process::Command;
use tokio::sync::Mutex;

#[derive(Debug, Default)]
pub(super) struct CliResolver {
    auth_state: Mutex<HashMap<BTreeSet<String>, CliAuthState>>,
}

#[derive(Debug, Clone, Default)]
enum CliAuthState {
    #[default]
    Unknown,
    Authenticated,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum AuthCheck {
    Authenticated,
    SignedOut,
    Failed(String),
}

enum CliCommandError {
    NotFound,
    Failed(String),
    Other(std::io::Error),
}

impl CliResolver {
    /// Resolve a single reference through the `op` CLI.
    pub(super) async fn resolve(
        &self,
        request: ResolveSecret<'_>,
        reference: &str,
    ) -> Result<String, SecretError> {
        let name = request.name;
        tracing::debug!(name = name, reference = reference, "1Password CLI read");

        match Self::read_reference(request, reference).await {
            Ok(secret) => Ok(secret),
            Err(CliCommandError::NotFound) => {
                Err(SecretError::resolution_failed(name, missing_op_message()))
            }
            Err(CliCommandError::Failed(details)) => Err(SecretError::resolution_failed(
                name,
                format!("op CLI failed: {details}"),
            )),
            Err(CliCommandError::Other(e)) => Err(SecretError::resolution_failed(
                name,
                format!("Failed to execute op CLI: {e}"),
            )),
        }
    }

    /// Ensure CLI auth is valid for the caller's environment policy.
    ///
    /// Returns `Some(secret_value)` when auth bootstrap consumed the current
    /// secret read, allowing caller to skip a second `op read` call.
    pub(super) async fn ensure_authenticated(
        &self,
        request: ResolveSecret<'_>,
        reference: &str,
    ) -> Result<Option<String>, SecretError> {
        let name = request.name;
        let mut states = self.auth_state.lock().await;
        let state = states.entry(request.withheld_env.clone()).or_default();
        match state {
            CliAuthState::Authenticated => return Ok(None),
            CliAuthState::Failed(message) => {
                return Err(SecretError::resolution_failed(name, message.clone()));
            }
            CliAuthState::Unknown => {}
        }

        match Self::check_auth(request).await {
            Ok(AuthCheck::Authenticated) => {
                *state = CliAuthState::Authenticated;
                Ok(None)
            }
            Ok(AuthCheck::SignedOut) => {
                Self::bootstrap_signed_out_session(request, reference, state).await
            }
            Ok(AuthCheck::Failed(details)) | Err(CliCommandError::Failed(details)) => fail_auth(
                name,
                state,
                format!(
                    "1Password CLI authentication check failed (`op whoami`). \
                    Run `op signin` and retry. Details: {details}"
                ),
            ),
            Err(CliCommandError::NotFound) => fail_auth(name, state, missing_op_message()),
            Err(CliCommandError::Other(e)) => fail_auth(
                name,
                state,
                format!(
                    "Failed to execute 1Password CLI authentication check (`op whoami`): {e}. \
                    Run `op signin` and retry."
                ),
            ),
        }
    }

    async fn check_auth(request: ResolveSecret<'_>) -> Result<AuthCheck, CliCommandError> {
        let output = command_output(Command::new("op").arg("whoami"), request).await?;

        if output.status.success() {
            return Ok(AuthCheck::Authenticated);
        }

        // The error output is read to tell "signed out" from other failures,
        // but never quoted (see `exit_details`).
        if String::from_utf8_lossy(&output.stderr)
            .to_lowercase()
            .contains("not signed in")
        {
            Ok(AuthCheck::SignedOut)
        } else {
            Ok(AuthCheck::Failed(exit_details(output.status)))
        }
    }

    async fn bootstrap_signed_out_session(
        request: ResolveSecret<'_>,
        reference: &str,
        state: &mut CliAuthState,
    ) -> Result<Option<String>, SecretError> {
        let name = request.name;
        match Self::read_reference(request, reference).await {
            Ok(secret) => {
                *state = CliAuthState::Authenticated;
                Ok(Some(secret))
            }
            Err(CliCommandError::NotFound) => fail_auth(name, state, missing_op_message()),
            Err(CliCommandError::Failed(details)) => fail_auth(
                name,
                state,
                format!(
                    "1Password CLI authentication check failed (`op whoami`) and \
                    bootstrap secret read failed. Run `op signin` and retry. \
                    Details: {details}"
                ),
            ),
            Err(CliCommandError::Other(e)) => fail_auth(
                name,
                state,
                format!(
                    "Failed to execute 1Password bootstrap secret read (`op read`): {e}. \
                    Run `op signin` and retry."
                ),
            ),
        }
    }

    async fn read_reference(
        request: ResolveSecret<'_>,
        reference: &str,
    ) -> Result<String, CliCommandError> {
        let output = command_output(Command::new("op").args(["read", reference]), request).await?;

        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).trim().to_string());
        }

        Err(CliCommandError::Failed(exit_details(output.status)))
    }
}

async fn command_output(
    command: &mut Command,
    request: ResolveSecret<'_>,
) -> Result<std::process::Output, CliCommandError> {
    request.withhold_from(command);
    command.output().await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            CliCommandError::NotFound
        } else {
            CliCommandError::Other(e)
        }
    })
}

fn fail_auth(
    name: &str,
    state: &mut CliAuthState,
    message: String,
) -> Result<Option<String>, SecretError> {
    *state = CliAuthState::Failed(message.clone());
    Err(SecretError::resolution_failed(name, message))
}

/// How a failed `op` run is described in an error: its exit status only.
///
/// The CLI's error output is deliberately not quoted: it can echo the
/// reference, the credentials in use or part of the secret, and nothing is
/// registered for redaction yet, because the secret was never resolved. Run
/// `op` yourself to see its output.
fn exit_details(status: std::process::ExitStatus) -> String {
    let exit = status.code().map_or_else(
        || "was terminated by a signal".to_string(),
        |code| format!("exited with status {code}"),
    );
    format!(
        "the 1Password CLI {exit}; its error output is not shown because it can contain secret \
         material (run `op` yourself to see it)"
    )
}

fn missing_op_message() -> String {
    "1Password CLI not found (`op` command unavailable). Install the 1Password CLI and retry."
        .to_string()
}
