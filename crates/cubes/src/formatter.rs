//! Code formatting integration
//!
//! This module provides formatting capabilities for various programming languages.

use crate::cube::FormatConfig;
use crate::{CodegenError, Result};

/// Language-specific code formatter
#[derive(Debug)]
pub struct Formatter;

impl Formatter {
    /// Create a new formatter
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Format code content based on language and configuration
    ///
    /// # Errors
    ///
    /// Returns an error if formatting fails
    pub fn format(&self, content: &str, language: &str, config: &FormatConfig) -> Result<String> {
        match language {
            "json" => self.format_json(content, config),
            "typescript" | "javascript" => {
                // For now, return content as-is
                // In a full implementation, we'd integrate with prettier or biome
                Ok(content.to_string())
            }
            "rust" => {
                // For now, return content as-is
                // In a full implementation, we'd integrate with rustfmt
                Ok(content.to_string())
            }
            _ => Ok(content.to_string()),
        }
    }

    /// Format JSON content
    #[allow(clippy::unused_self)] // Will use self for formatting state in future
    fn format_json(&self, content: &str, config: &FormatConfig) -> Result<String> {
        let value: serde_json::Value = serde_json::from_str(content)?;

        let indent_size = config.indent_size.unwrap_or(2);
        let indent_char = if config.indent == "tab" { '\t' } else { ' ' };

        let formatted = if indent_char == '\t' {
            serde_json::to_string_pretty(&value)?
        } else {
            let mut buf = Vec::new();
            let indent = vec![b' '; indent_size];
            let formatter = serde_json::ser::PrettyFormatter::with_indent(indent.as_slice());
            let mut ser = serde_json::Serializer::with_formatter(&mut buf, formatter);
            serde::Serialize::serialize(&value, &mut ser)
                .map_err(|e| CodegenError::Formatting(e.to_string()))?;
            String::from_utf8(buf).map_err(|e| CodegenError::Formatting(e.to_string()))?
        };

        Ok(formatted)
    }
}

impl Default for Formatter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_json() {
        let fmt = Formatter::new();
        let input = r#"{"name":"test","value":123}"#;
        let config = FormatConfig {
            indent: "space".to_string(),
            indent_size: Some(2),
            ..Default::default()
        };

        let result = fmt.format(input, "json", &config);
        assert!(result.is_ok());

        let output = result.unwrap();
        assert!(output.contains("  ")); // Should have 2-space indentation
        assert!(output.contains("\"name\": \"test\""));
    }

    #[test]
    fn test_format_unknown_language() {
        let formatter = Formatter::new();
        let input = "some content";
        let config = FormatConfig::default();

        let result = formatter.format(input, "unknown", &config);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), input);
    }
}
