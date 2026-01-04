//! Trace testing infrastructure for cuenv acceptance tests
//!
//! Provides span collection and assertion helpers for verifying tracing behavior.

// These are test utilities - dead code and unused imports are expected
#![allow(dead_code)]
#![allow(unused_imports)]

mod assertions;
mod collector;

pub use assertions::{assert_span_count, assert_span_exists, assert_span_has_field};
pub use collector::{CapturedSpan, SpanCollector};
