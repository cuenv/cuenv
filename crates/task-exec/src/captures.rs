//! Capture resolution for extracting regex matches from task output.

use super::{CaptureSource, TaskCapture};
use regex::Regex;
use std::collections::HashMap;

/// Extract named captures from task stdout/stderr using regex patterns.
///
/// Each capture definition specifies a regex pattern with at least one capture group.
/// The first capture group's match becomes the named value.
///
/// Invalid regex patterns or captures without a group 1 match are skipped
/// with a warning logged via tracing.
///
/// Returns a map of capture name -> extracted value.
pub fn resolve_captures(
    captures: &HashMap<String, TaskCapture>,
    stdout: &str,
    stderr: &str,
) -> HashMap<String, String> {
    let mut results = HashMap::new();

    for (name, cap) in captures {
        let source = match cap.source {
            CaptureSource::Stdout => stdout,
            CaptureSource::Stderr => stderr,
        };

        let re = match Regex::new(&cap.pattern) {
            Ok(re) => re,
            Err(err) => {
                tracing::warn!(
                    capture = name,
                    pattern = cap.pattern,
                    error = %err,
                    "Failed to compile capture regex"
                );
                continue;
            }
        };

        match re.captures(source).and_then(|caps| caps.get(1)) {
            Some(m) => {
                results.insert(name.clone(), m.as_str().trim().to_string());
            }
            None => {
                tracing::debug!(
                    capture = name,
                    pattern = cap.pattern,
                    "No capture group 1 match found"
                );
            }
        }
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_capture(pattern: &str, source: CaptureSource) -> TaskCapture {
        TaskCapture {
            pattern: pattern.to_string(),
            source,
        }
    }

    #[test]
    fn test_resolve_captures_from_stdout() {
        let captures = HashMap::from([(
            "previewUrl".to_string(),
            make_capture(r"Version Preview URL: (.+)", CaptureSource::Stdout),
        )]);

        let stdout = "Uploading...\nVersion Preview URL: https://example.workers.dev\nDone.";

        let result = resolve_captures(&captures, stdout, "");
        assert_eq!(
            result.get("previewUrl").map(String::as_str),
            Some("https://example.workers.dev")
        );
    }

    #[test]
    fn test_resolve_captures_from_stderr() {
        let captures = HashMap::from([(
            "errorCode".to_string(),
            make_capture(r"Error code: (\d+)", CaptureSource::Stderr),
        )]);

        let result = resolve_captures(&captures, "", "Error code: 42\nFailed.");
        assert_eq!(result.get("errorCode").map(String::as_str), Some("42"));
    }

    #[test]
    fn test_resolve_captures_no_match() {
        let captures = HashMap::from([(
            "missing".to_string(),
            make_capture(r"not found: (.+)", CaptureSource::Stdout),
        )]);

        let result = resolve_captures(&captures, "nothing here", "");
        assert!(result.is_empty());
    }

    #[test]
    fn test_resolve_captures_multiple() {
        let captures = HashMap::from([
            (
                "version".to_string(),
                make_capture(r"version: (.+)", CaptureSource::Stdout),
            ),
            (
                "url".to_string(),
                make_capture(r"URL: (.+)", CaptureSource::Stdout),
            ),
        ]);

        let stdout = "version: 1.2.3\nURL: https://example.com";
        let result = resolve_captures(&captures, stdout, "");
        assert_eq!(result.len(), 2);
        assert_eq!(result.get("version").map(String::as_str), Some("1.2.3"));
        assert_eq!(
            result.get("url").map(String::as_str),
            Some("https://example.com")
        );
    }

    #[test]
    fn test_resolve_captures_empty_output() {
        let captures = HashMap::from([(
            "url".to_string(),
            make_capture(r"URL: (.+)", CaptureSource::Stdout),
        )]);

        let result = resolve_captures(&captures, "", "");
        assert!(result.is_empty());
    }

    #[test]
    fn test_resolve_captures_invalid_regex() {
        let captures = HashMap::from([(
            "bad".to_string(),
            make_capture(r"[invalid(", CaptureSource::Stdout),
        )]);

        let result = resolve_captures(&captures, "anything", "");
        assert!(result.is_empty());
    }

    #[test]
    fn test_resolve_captures_trims_whitespace() {
        let captures = HashMap::from([(
            "val".to_string(),
            make_capture(r"value: (.+)", CaptureSource::Stdout),
        )]);

        let result = resolve_captures(&captures, "value:  hello  ", "");
        assert_eq!(result.get("val").map(String::as_str), Some("hello"));
    }
}
