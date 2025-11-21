---
files: ["**/tests/**/*", "**/*_test.rs", "**/*.feature"]
description: Instructions for writing and running tests
---

# Testing Instructions

## Test Organization

### Directory Structure
```
cuenv/
├── crates/
│   ├── cuengine/
│   │   ├── src/           # Unit tests inline with code
│   │   └── tests/         # Integration tests
│   ├── cuenv-core/
│   │   ├── src/
│   │   └── tests/
│   └── cuenv-cli/
│       ├── src/
│       └── tests/
└── tests/                 # Workspace-level tests
    ├── integration_tests.rs
    ├── ffi_edge_cases.rs
    └── features/          # BDD tests
```

## Test Types

### Unit Tests
Place unit tests in the same file as the code:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_function_name() {
        // Arrange
        let input = "test";
        
        // Act
        let result = function_under_test(input);
        
        // Assert
        assert_eq!(result, expected);
    }
}
```

### Integration Tests
Place in `tests/` directory:
```rust
// tests/integration_tests.rs
use cuengine::CueEngine;

#[test]
fn test_end_to_end_workflow() {
    let engine = CueEngine::new();
    // Test complete workflows
}
```

### BDD Tests (Cucumber)
Feature files in Gherkin syntax:
```gherkin
# tests/features/evaluation.feature
Feature: CUE Evaluation
  Scenario: Evaluate simple configuration
    Given a CUE file with content
      """
      package test
      value: 42
      """
    When I evaluate the file
    Then the result should contain "value": 42
```

## Running Tests

### Standard Test Commands
```bash
# Run all tests (takes ~45-60 seconds)
cargo test --workspace

# Run only library tests (faster, ~30 seconds)
cargo test --lib --workspace

# Run specific crate tests
cargo test -p cuengine
cargo test -p cuenv-core
cargo test -p cuenv-cli

# Run specific test
cargo test test_name

# Run with output
cargo test -- --nocapture

# Run with specific test pattern
cargo test integration_
```

### BDD Tests
```bash
# Run BDD tests
cargo test --test bdd

# Run specific feature
cargo test --test bdd -- --name evaluation
```

### Performance Tests
```bash
# Run benchmarks (takes 60+ seconds)
cargo bench --workspace --no-fail-fast

# Run specific benchmark
cargo bench --bench evaluation_benchmarks
```

## Test Requirements

### Coverage
- All public APIs must have tests
- Edge cases and error conditions must be tested
- Aim for >80% code coverage
- Critical paths should have 100% coverage

### Test Quality
- Use descriptive test names that explain what is being tested
- Follow Arrange-Act-Assert pattern
- One assertion focus per test
- Use test fixtures for complex setups
- Mock external dependencies

### Error Testing
```rust
#[test]
fn test_error_handling() {
    let result = function_that_fails("invalid");
    assert!(result.is_err());
    
    let err = result.unwrap_err();
    assert!(err.to_string().contains("expected message"));
}
```

### Property-Based Testing
Use `proptest` for complex logic:
```rust
use proptest::prelude::*;

proptest! {
    #[test]
    fn test_property(input in any::<String>()) {
        // Test that property holds for all inputs
        let result = function(input.clone());
        prop_assert!(validate_property(result));
    }
}
```

## Test Data

### Location
```
tests/
├── fixtures/          # Test data files
│   ├── valid/        # Valid test cases
│   └── invalid/      # Invalid test cases
└── helpers/          # Test utilities
```

### Using Test Fixtures
```rust
#[test]
fn test_with_fixture() {
    let fixture_path = Path::new("tests/fixtures/valid/sample.cue");
    let content = fs::read_to_string(fixture_path).unwrap();
    // Use fixture in test
}
```

### Temporary Files
```rust
use tempfile::tempdir;

#[test]
fn test_with_temp_file() {
    let dir = tempdir().unwrap();
    let file_path = dir.path().join("test.cue");
    // Work with temporary file
    // Automatically cleaned up when dir drops
}
```

## Async Tests

### Using tokio-test
```rust
use tokio_test::block_on;

#[test]
fn test_async_function() {
    let result = block_on(async {
        async_function().await
    });
    assert_eq!(result, expected);
}
```

### Using tokio::test macro
```rust
#[tokio::test]
async fn test_async_with_macro() {
    let result = async_function().await;
    assert_eq!(result, expected);
}
```

## Test Best Practices

### Do's
- ✅ Write tests before or alongside code (TDD/BDD)
- ✅ Test both success and failure paths
- ✅ Use meaningful test names
- ✅ Keep tests focused and simple
- ✅ Use test helpers for common setup
- ✅ Run tests frequently during development
- ✅ Fix failing tests immediately

### Don'ts
- ❌ Don't test implementation details
- ❌ Don't make tests dependent on each other
- ❌ Don't use sleeps for timing (use proper async)
- ❌ Don't ignore failing tests
- ❌ Don't commit code without running tests
- ❌ Don't skip edge cases

## CI/CD Testing

### Pre-commit Checklist
```bash
# Format code
cargo fmt

# Run clippy
cargo clippy --workspace --all-targets --all-features -- -D warnings

# Run tests
cargo test --workspace
```

### CI Test Matrix
The CI runs tests on:
- Ubuntu + macOS
- Rust stable + MSRV (1.85.0)
- All feature combinations

### Expected Timing
- Unit tests: ~30 seconds
- Integration tests: ~45 seconds
- Full test suite: ~45-60 seconds
- Benchmarks: ~60+ seconds

## Troubleshooting

### FFI Tests Failing
Go FFI tests may fail without CGO:
```bash
# Check Go/CGO availability
go version
echo $CGO_ENABLED  # Should be 1
```

### Slow Tests
```bash
# Run only fast tests
cargo test --lib --workspace

# Skip slow integration tests
cargo test --workspace --exclude-test-name slow_
```

### Debugging Test Failures
```bash
# Show all output
cargo test -- --nocapture

# Show test threads
cargo test -- --test-threads=1

# Enable tracing
RUST_LOG=debug cargo test
```
