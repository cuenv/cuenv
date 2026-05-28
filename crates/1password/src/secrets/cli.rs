use cuenv_secrets::SecretError;
use tokio::{process::Command, sync::Mutex};

#[derive(Debug)]
pub(super) struct CliResolver {
    auth_state: Mutex<CliAuthState>,
}

#[derive(Debug, Clone)]
enum CliAuthState {
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

impl Default for CliResolver {
    fn default() -> Self {
        Self {
            auth_state: Mutex::new(CliAuthState::Unknown),
        }
    }
}

impl CliResolver {
    /// Resolve a single reference through the `op` CLI.
    pub(super) async fn resolve(&self, name: &str, reference: &str) -> Result<String, SecretError> {
        tracing::debug!(name = name, reference = reference, "1Password CLI read");

        match Self::read_reference(reference).await {
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

    /// Ensure CLI auth is valid once per resolver instance.
    ///
    /// Returns `Some(secret_value)` when auth bootstrap consumed the current
    /// secret read, allowing caller to skip a second `op read` call.
    pub(super) async fn ensure_authenticated(
        &self,
        name: &str,
        reference: &str,
    ) -> Result<Option<String>, SecretError> {
        let mut state = self.auth_state.lock().await;
        match &*state {
            CliAuthState::Authenticated => return Ok(None),
            CliAuthState::Failed(message) => {
                return Err(SecretError::resolution_failed(name, message.clone()));
            }
            CliAuthState::Unknown => {}
        }

        match Self::check_auth().await {
            Ok(AuthCheck::Authenticated) => {
                *state = CliAuthState::Authenticated;
                Ok(None)
            }
            Ok(AuthCheck::SignedOut) => {
                self.bootstrap_signed_out_session(name, reference, &mut state)
                    .await
            }
            Ok(AuthCheck::Failed(details)) | Err(CliCommandError::Failed(details)) => fail_auth(
                name,
                &mut state,
                format!(
                    "1Password CLI authentication check failed (`op whoami`). \
                    Run `op signin` and retry. Details: {details}"
                ),
            ),
            Err(CliCommandError::NotFound) => fail_auth(name, &mut state, missing_op_message()),
            Err(CliCommandError::Other(e)) => fail_auth(
                name,
                &mut state,
                format!(
                    "Failed to execute 1Password CLI authentication check (`op whoami`): {e}. \
                    Run `op signin` and retry."
                ),
            ),
        }
    }

    async fn check_auth() -> Result<AuthCheck, CliCommandError> {
        let output = command_output(Command::new("op").arg("whoami")).await?;

        if output.status.success() {
            return Ok(AuthCheck::Authenticated);
        }

        let details = stderr_details(&output.stderr);
        if details.to_lowercase().contains("not signed in") {
            Ok(AuthCheck::SignedOut)
        } else {
            Ok(AuthCheck::Failed(details))
        }
    }

    async fn bootstrap_signed_out_session(
        &self,
        name: &str,
        reference: &str,
        state: &mut CliAuthState,
    ) -> Result<Option<String>, SecretError> {
        match Self::read_reference(reference).await {
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

    async fn read_reference(reference: &str) -> Result<String, CliCommandError> {
        let output = command_output(Command::new("op").args(["read", reference])).await?;

        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).trim().to_string());
        }

        Err(CliCommandError::Failed(stderr_details(&output.stderr)))
    }
}

async fn command_output(command: &mut Command) -> Result<std::process::Output, CliCommandError> {
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

fn stderr_details(stderr: &[u8]) -> String {
    let details = String::from_utf8_lossy(stderr).trim().to_string();
    if details.is_empty() {
        "no error output from 1Password CLI".to_string()
    } else {
        details
    }
}

fn missing_op_message() -> String {
    "1Password CLI not found (`op` command unavailable). Install the 1Password CLI and retry."
        .to_string()
}
