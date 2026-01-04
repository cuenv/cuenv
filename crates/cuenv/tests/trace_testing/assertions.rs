//! Span assertion helpers for trace verification
//!
//! Provides test assertions for verifying tracing span behavior.

// Test assertions use panic! intentionally
#![allow(clippy::panic)]

use super::collector::CapturedSpan;

/// Assert that a span with the given name was captured
///
/// # Panics
///
/// Panics if no span with the given name exists
pub fn assert_span_exists(spans: &[CapturedSpan], name: &str) {
    assert!(
        spans.iter().any(|s| s.name == name),
        "Expected span '{}' not found. Found spans: {:?}",
        name,
        spans.iter().map(|s| &s.name).collect::<Vec<_>>()
    );
}

/// Assert that a span with the given name has a specific field
///
/// # Panics
///
/// Panics if the span doesn't exist or doesn't have the field
pub fn assert_span_has_field(spans: &[CapturedSpan], name: &str, field: &str) {
    let span = spans.iter().find(|s| s.name == name).unwrap_or_else(|| {
        panic!(
            "Span '{}' not found. Found spans: {:?}",
            name,
            spans.iter().map(|s| &s.name).collect::<Vec<_>>()
        )
    });

    assert!(
        span.fields.iter().any(|(k, _)| k == field),
        "Span '{}' missing field '{}'. Available fields: {:?}",
        name,
        field,
        span.fields.iter().map(|(k, _)| k).collect::<Vec<_>>()
    );
}

/// Assert that a specific number of spans with the given name were captured
///
/// # Panics
///
/// Panics if the count doesn't match
pub fn assert_span_count(spans: &[CapturedSpan], name: &str, expected: usize) {
    let count = spans.iter().filter(|s| s.name == name).count();
    assert_eq!(
        count,
        expected,
        "Expected {} '{}' spans, found {}. All spans: {:?}",
        expected,
        name,
        count,
        spans.iter().map(|s| &s.name).collect::<Vec<_>>()
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing::Level;

    fn make_span(name: &str, fields: Vec<(&str, &str)>) -> CapturedSpan {
        CapturedSpan {
            name: name.to_string(),
            target: "test".to_string(),
            level: Level::INFO,
            fields: fields
                .into_iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    #[test]
    fn test_assert_span_exists_passes() {
        let spans = vec![make_span("test_span", vec![])];
        assert_span_exists(&spans, "test_span");
    }

    #[test]
    #[should_panic(expected = "Expected span 'missing' not found")]
    fn test_assert_span_exists_fails() {
        let spans = vec![make_span("other_span", vec![])];
        assert_span_exists(&spans, "missing");
    }

    #[test]
    fn test_assert_span_has_field_passes() {
        let spans = vec![make_span("test_span", vec![("path", "/test")])];
        assert_span_has_field(&spans, "test_span", "path");
    }

    #[test]
    #[should_panic(expected = "missing field 'missing'")]
    fn test_assert_span_has_field_fails() {
        let spans = vec![make_span("test_span", vec![("path", "/test")])];
        assert_span_has_field(&spans, "test_span", "missing");
    }

    #[test]
    fn test_assert_span_count_passes() {
        let spans = vec![
            make_span("task", vec![]),
            make_span("task", vec![]),
            make_span("other", vec![]),
        ];
        assert_span_count(&spans, "task", 2);
        assert_span_count(&spans, "other", 1);
    }

    #[test]
    #[should_panic(expected = "Expected 3 'task' spans, found 2")]
    fn test_assert_span_count_fails() {
        let spans = vec![make_span("task", vec![]), make_span("task", vec![])];
        assert_span_count(&spans, "task", 3);
    }
}
