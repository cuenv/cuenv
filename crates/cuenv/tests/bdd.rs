//! BDD tests for cuenv CLI using Cucumber
//!
//! These tests verify the behavior of the CLI through feature specifications,
//! particularly focusing on shell integration and hook execution.

mod bdd_support;

use bdd_support::TestWorld;
use cucumber::World;

// Main test runner for cucumber BDD tests
// Note: These tests are incompatible with nextest and should be run separately
// with: cargo test --test bdd
// See: https://github.com/cucumber-rs/cucumber/issues/370
#[tokio::main]
async fn main() {
    // Helper for nextest compatibility
    // Nextest runs with --list --format terse to discover tests
    // Since we run these tests separately, we can just ignore this command
    if std::env::args().any(|arg| arg == "--list") {
        return;
    }

    TestWorld::cucumber().run("tests/bdd/features/").await;
}
