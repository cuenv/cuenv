use miette::Diagnostic;
use thiserror::Error;

#[derive(Error, Debug, Diagnostic)]
pub enum RemoteError {
    #[error("Remote execution failed")]
    #[diagnostic(code(cuenv_remote::execution_failed))]
    ExecutionFailed,
}
