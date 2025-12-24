//! Log Redaction
//!
//! Provides secret redaction for stdout/stderr streams to prevent
//! accidental secret exposure in CI logs.

use std::collections::HashSet;

/// Minimum secret length to redact (shorter secrets may cause false positives)
pub const MIN_SECRET_LENGTH: usize = 4;

/// Placeholder for redacted secrets
pub const REDACTED_PLACEHOLDER: &str = "[REDACTED]";

/// Warning for short secrets that won't be redacted
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShortSecretWarning {
    /// Secret key name
    pub key: String,
    /// Actual length
    pub length: usize,
}

/// Log redactor that replaces secret values with placeholders
///
/// Uses a sliding window buffer to handle secrets that span chunk boundaries.
///
/// # Streaming Usage
///
/// When processing streaming input via [`redact`], the redactor buffers content
/// to detect secrets that span chunk boundaries. **Callers must call [`flush`]
/// when the stream ends** to retrieve any remaining buffered content.
///
/// ```ignore
/// let (mut redactor, _) = LogRedactor::new(secrets);
/// for chunk in stream {
///     let redacted = redactor.redact(&chunk);
///     output.push_str(&redacted);
/// }
/// // IMPORTANT: Don't forget to flush!
/// output.push_str(&redactor.flush());
/// ```
///
/// For complete strings where buffering isn't needed, use [`redact_immediate`] instead.
///
/// [`redact`]: LogRedactor::redact
/// [`flush`]: LogRedactor::flush
/// [`redact_immediate`]: LogRedactor::redact_immediate
#[derive(Debug)]
pub struct LogRedactor {
    /// Secret values to redact (sorted by length descending for greedy matching)
    secrets: Vec<String>,
    /// Buffer for handling cross-boundary secrets
    buffer: String,
    /// Maximum secret length (determines buffer size)
    max_secret_len: usize,
}

impl LogRedactor {
    /// Create a new log redactor with the given secret values
    ///
    /// # Arguments
    /// * `secrets` - Secret values to redact
    ///
    /// # Returns
    /// Tuple of (redactor, warnings for short secrets)
    #[must_use]
    pub fn new(secrets: Vec<String>) -> (Self, Vec<ShortSecretWarning>) {
        let mut warnings = Vec::new();
        let mut valid_secrets: Vec<String> = Vec::new();

        for (idx, secret) in secrets.into_iter().enumerate() {
            if secret.len() < MIN_SECRET_LENGTH {
                warnings.push(ShortSecretWarning {
                    key: format!("secret_{idx}"),
                    length: secret.len(),
                });
            } else {
                valid_secrets.push(secret);
            }
        }

        // Sort by length descending for greedy matching (longer secrets first)
        valid_secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));

        let max_secret_len = valid_secrets.iter().map(String::len).max().unwrap_or(0);

        (
            Self {
                secrets: valid_secrets,
                buffer: String::new(),
                max_secret_len,
            },
            warnings,
        )
    }

    /// Create a redactor with named secrets for better warnings
    ///
    /// # Arguments
    /// * `secrets` - Map of secret names to values
    #[must_use]
    pub fn with_names(
        secrets: impl IntoIterator<Item = (String, String)>,
    ) -> (Self, Vec<ShortSecretWarning>) {
        let mut warnings = Vec::new();
        let mut valid_secrets: Vec<String> = Vec::new();

        for (key, value) in secrets {
            if value.len() < MIN_SECRET_LENGTH {
                warnings.push(ShortSecretWarning {
                    key,
                    length: value.len(),
                });
            } else {
                valid_secrets.push(value);
            }
        }

        // Sort by length descending for greedy matching
        valid_secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));

        // Deduplicate secrets (same value may appear under different names)
        let unique: HashSet<String> = valid_secrets.into_iter().collect();
        let mut valid_secrets: Vec<String> = unique.into_iter().collect();
        valid_secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));

        let max_secret_len = valid_secrets.iter().map(String::len).max().unwrap_or(0);

        (
            Self {
                secrets: valid_secrets,
                buffer: String::new(),
                max_secret_len,
            },
            warnings,
        )
    }

    /// Redact secrets from the input string
    ///
    /// This method handles streaming input by buffering to catch secrets
    /// that span chunk boundaries.
    ///
    /// # Arguments
    /// * `input` - Input chunk to process
    ///
    /// # Returns
    /// Redacted output (may be shorter than input due to buffering)
    pub fn redact(&mut self, input: &str) -> String {
        if self.secrets.is_empty() {
            return input.to_string();
        }

        // Append input to buffer
        self.buffer.push_str(input);

        // Keep enough in buffer to catch spanning secrets (2x max length)
        let buffer_threshold = self.max_secret_len * 2;

        if self.buffer.len() <= buffer_threshold {
            // Not enough data yet, return empty and keep buffering
            return String::new();
        }

        // Process all but the last buffer_threshold bytes
        let process_len = self.buffer.len() - buffer_threshold;
        let to_process: String = self.buffer.drain(..process_len).collect();

        self.redact_immediate(&to_process)
    }

    /// Flush any remaining buffered content
    ///
    /// Call this when the stream ends to get any remaining output.
    pub fn flush(&mut self) -> String {
        if self.buffer.is_empty() {
            return String::new();
        }

        let remaining = std::mem::take(&mut self.buffer);
        self.redact_immediate(&remaining)
    }

    /// Redact secrets immediately without buffering
    ///
    /// Use this for complete strings where buffering isn't needed.
    #[must_use]
    pub fn redact_immediate(&self, input: &str) -> String {
        let mut result = input.to_string();

        for secret in &self.secrets {
            result = result.replace(secret, REDACTED_PLACEHOLDER);
        }

        result
    }

    /// Check if any secrets are configured
    #[must_use]
    pub const fn has_secrets(&self) -> bool {
        !self.secrets.is_empty()
    }

    /// Get the number of secrets being redacted
    #[must_use]
    pub const fn secret_count(&self) -> usize {
        self.secrets.len()
    }
}

/// Redact secrets from a complete string (convenience function)
///
/// # Arguments
/// * `input` - String to redact
/// * `secrets` - Secret values to redact
#[must_use]
pub fn redact_secrets(input: &str, secrets: &[String]) -> String {
    if secrets.is_empty() {
        return input.to_string();
    }

    let mut result = input.to_string();

    // Sort by length descending for greedy matching
    let mut sorted_secrets: Vec<&String> = secrets.iter().collect();
    sorted_secrets.sort_by_key(|s| std::cmp::Reverse(s.len()));

    for secret in sorted_secrets {
        if secret.len() >= MIN_SECRET_LENGTH {
            result = result.replace(secret.as_str(), REDACTED_PLACEHOLDER);
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_redaction() {
        let (redactor, _) = LogRedactor::new(vec!["secret123".to_string()]);
        let result = redactor.redact_immediate("The password is secret123, don't share it");
        assert_eq!(result, "The password is [REDACTED], don't share it");
    }

    #[test]
    fn test_multiple_secrets() {
        let (redactor, _) =
            LogRedactor::new(vec!["password123".to_string(), "api_key_xyz".to_string()]);
        let result = redactor.redact_immediate("password123 and api_key_xyz are both secrets");
        assert_eq!(result, "[REDACTED] and [REDACTED] are both secrets");
    }

    #[test]
    fn test_repeated_secret() {
        let (redactor, _) = LogRedactor::new(vec!["secret".to_string()]);
        let result = redactor.redact_immediate("secret appears twice: secret");
        assert_eq!(result, "[REDACTED] appears twice: [REDACTED]");
    }

    #[test]
    fn test_short_secret_warning() {
        let (redactor, warnings) = LogRedactor::new(vec![
            "ab".to_string(),   // Too short
            "abc".to_string(),  // Too short
            "abcd".to_string(), // Just right
        ]);

        assert_eq!(warnings.len(), 2);
        assert_eq!(redactor.secret_count(), 1);
    }

    #[test]
    fn test_named_secrets_warning() {
        let secrets = vec![
            ("DB_PASS".to_string(), "longpassword".to_string()),
            ("SHORT".to_string(), "ab".to_string()),
        ];
        let (_, warnings) = LogRedactor::with_names(secrets);

        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].key, "SHORT");
        assert_eq!(warnings[0].length, 2);
    }

    #[test]
    fn test_streaming_redaction() {
        let (mut redactor, _) = LogRedactor::new(vec!["secretpassword".to_string()]);

        // Simulate streaming chunks where secret spans boundary
        let chunk1 = "The password is secret";
        let chunk2 = "password which is bad";

        let out1 = redactor.redact(chunk1);
        let out2 = redactor.redact(chunk2);
        let out3 = redactor.flush();

        let combined = format!("{out1}{out2}{out3}");
        assert!(combined.contains("[REDACTED]"));
        assert!(!combined.contains("secretpassword"));
    }

    #[test]
    fn test_no_secrets() {
        let (redactor, warnings) = LogRedactor::new(vec![]);
        assert!(warnings.is_empty());
        assert!(!redactor.has_secrets());

        let result = redactor.redact_immediate("nothing to redact here");
        assert_eq!(result, "nothing to redact here");
    }

    #[test]
    fn test_greedy_matching() {
        // Longer secret should be matched first
        let (redactor, _) = LogRedactor::new(vec!["pass".to_string(), "password".to_string()]);
        let result = redactor.redact_immediate("the password is set");
        // Should redact "password" not just "pass"
        assert_eq!(result, "the [REDACTED] is set");
    }

    #[test]
    fn test_redact_secrets_function() {
        let secrets = vec!["mysecret".to_string(), "another".to_string()];
        let result = redact_secrets("mysecret and another value", &secrets);
        assert_eq!(result, "[REDACTED] and [REDACTED] value");
    }

    #[test]
    fn test_empty_input() {
        let (redactor, _) = LogRedactor::new(vec!["secret".to_string()]);
        let result = redactor.redact_immediate("");
        assert_eq!(result, "");
    }

    #[test]
    fn test_secret_at_boundaries() {
        let (redactor, _) = LogRedactor::new(vec!["secret".to_string()]);

        // Secret at start
        let result = redactor.redact_immediate("secret is here");
        assert_eq!(result, "[REDACTED] is here");

        // Secret at end
        let result = redactor.redact_immediate("here is secret");
        assert_eq!(result, "here is [REDACTED]");
    }

    #[test]
    fn test_duplicate_secrets_deduplicated() {
        let secrets = vec![
            ("KEY1".to_string(), "samevalue".to_string()),
            ("KEY2".to_string(), "samevalue".to_string()),
        ];
        let (redactor, _) = LogRedactor::with_names(secrets);

        // Should only have one secret after deduplication
        assert_eq!(redactor.secret_count(), 1);
    }

    #[test]
    fn test_special_characters() {
        let (redactor, _) = LogRedactor::new(vec!["pass$word!@#".to_string()]);
        let result = redactor.redact_immediate("the pass$word!@# is special");
        assert_eq!(result, "the [REDACTED] is special");
    }

    #[test]
    fn test_multiline_content() {
        let (redactor, _) = LogRedactor::new(vec!["secretkey".to_string()]);
        let input = "line1\nsecretkey\nline3";
        let result = redactor.redact_immediate(input);
        assert_eq!(result, "line1\n[REDACTED]\nline3");
    }
}
