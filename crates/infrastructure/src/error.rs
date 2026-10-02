//! Error type for the infrastructure engine.

use thiserror::Error;

/// Result alias for this crate.
pub type Result<Success> = std::result::Result<Success, InfrastructureError>;

/// Errors raised while planning or applying infrastructure.
#[derive(Debug, Error)]
pub enum InfrastructureError {
    /// Invalid `infrastructure` configuration.
    #[error("infrastructure configuration error: {0}")]
    Configuration(String),

    /// Failure encoding or decoding Terraform values.
    #[error("value codec error: {0}")]
    Codec(String),

    /// Failure launching or talking to a provider plugin process.
    #[error("provider plugin error: {0}")]
    Plugin(String),

    /// A gRPC call to a provider failed.
    #[error("provider procedure {method} failed: {status}")]
    RemoteProcedure {
        /// Procedure path.
        method: String,
        /// gRPC status returned by the provider.
        status: Box<tonic::Status>,
    },

    /// Failure downloading or installing a provider from a registry.
    #[error("provider install error: {0}")]
    Install(String),

    /// Input or output failure.
    #[error("{context}: {source}")]
    InputOutput {
        /// What cuenv was doing.
        context: String,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
}

impl InfrastructureError {
    /// Build a configuration error.
    #[must_use]
    pub fn configuration(message: impl Into<String>) -> Self {
        Self::Configuration(message.into())
    }

    /// Build a codec error.
    #[must_use]
    pub fn codec(message: impl Into<String>) -> Self {
        Self::Codec(message.into())
    }

    /// Build a plugin error.
    #[must_use]
    pub fn plugin(message: impl Into<String>) -> Self {
        Self::Plugin(message.into())
    }

    /// Build an install error.
    #[must_use]
    pub fn install(message: impl Into<String>) -> Self {
        Self::Install(message.into())
    }

    /// Build an input or output error with context.
    ///
    /// Never pass a `serde_json` error's message into any error: it can
    /// quote the value it failed on. Use `json_error_category` instead.
    #[must_use]
    pub fn input_output(context: impl Into<String>, source: std::io::Error) -> Self {
        Self::InputOutput {
            context: context.into(),
            source,
        }
    }
}

/// Name a JSON error's category without its message, which can quote the
/// value (possibly a secret) it failed on.
pub(crate) fn json_error_category(error: &serde_json::Error) -> &'static str {
    match error.classify() {
        serde_json::error::Category::Io => "input or output error",
        serde_json::error::Category::Syntax => "syntax error",
        serde_json::error::Category::Data => "data error",
        serde_json::error::Category::Eof => "unexpected end",
    }
}

/// Remove control characters (C0, DEL and C1) from text to display.
///
/// Text from a provider or the state store must not move the cursor,
/// rewrite earlier output or change the terminal's state.
#[must_use]
pub fn strip_control_characters(text: &str) -> String {
    text.chars()
        .filter(|character| !character.is_control())
        .collect()
}

/// [`strip_control_characters`], keeping line breaks, for multi-line text
/// such as provider diagnostics.
#[must_use]
pub fn strip_control_characters_except_newlines(text: &str) -> String {
    text.chars()
        .filter(|character| *character == '\n' || !character.is_control())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_characters_are_stripped() {
        let hostile = "ok\u{1b}[2J\u{7}\u{7f}\u{9b}31m\tend\r\nnext";
        assert_eq!(strip_control_characters(hostile), "ok[2J31mendnext");
        assert_eq!(
            strip_control_characters_except_newlines(hostile),
            "ok[2J31mend\nnext"
        );
        assert_eq!(strip_control_characters("plain text é"), "plain text é");
    }
}
