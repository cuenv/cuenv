---
files: ["crates/**/*.rs"]
description: Instructions for working with Rust code in the cuenv project
---

# Rust Code Instructions

## Code Style and Conventions

### General Guidelines
- Use Rust 2024 edition (MSRV 1.85.0)
- Follow standard Rust naming conventions (snake_case for functions/variables, PascalCase for types)
- Prefer explicit error handling with `Result<T, E>` over panics
- Use `thiserror` for custom error types
- Use `tracing` for logging, not `println!` or `eprintln!`
- Document public APIs with doc comments (`///`)

### Error Handling
- Always propagate errors with `?` operator when appropriate
- Use descriptive error messages with context
- Prefer custom error types from `cuenv-core` for domain-specific errors
- Use `miette` for user-facing diagnostic errors

### Safety and Security
- Minimize use of `unsafe` code; justify when necessary
- All FFI boundaries must be carefully validated
- Use `#[must_use]` for important return values
- Validate all external inputs at system boundaries

## Testing

### Test Organization
- Unit tests: In the same file as implementation using `#[cfg(test)]`
- Integration tests: In `tests/` directory
- BDD tests: Using cucumber framework in `tests/*.feature` files

### Running Tests
```bash
# Run all tests
cargo test --workspace

# Run only library tests (faster, ~30 seconds)
cargo test --lib --workspace

# Run specific test
cargo test test_name

# Run with output
cargo test -- --nocapture
```

### Test Requirements
- Write tests for all public APIs
- Include edge cases and error conditions
- Use property-based testing with `proptest` for complex logic
- Mock FFI calls in tests to avoid Go dependencies

## Linting and Formatting

### Before Committing
```bash
# Format code
cargo fmt

# Run clippy with strict warnings
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

### Clippy Configuration
- Use `#[allow(clippy::lint_name)]` sparingly with justification comments
- Common allowed lints for FFI code:
  - `clippy::missing_safety_doc` (for internal unsafe functions)
  - `clippy::too_many_arguments` (for FFI signatures matching Go)

## FFI Specific Guidelines

### Working with Go Bridge
- Located in `crates/cuengine/bridge.go`
- FFI wrapper in `crates/cuengine/src/lib.rs`
- Always validate data crossing FFI boundaries
- Use proper RAII for memory management
- Document memory ownership clearly

### Build Process
- Go code is compiled via `build.rs` into a static library
- Requires Go 1.21+ and CGO enabled
- Build takes ~90 seconds on first run (caching helps)

## Performance Considerations
- Use `parking_lot` for locks instead of std mutexes
- Implement caching with LRU for expensive operations
- Profile with `cargo bench` before optimizing
- Consider zero-copy operations where possible

## Dependencies
- Prefer workspace dependencies from root `Cargo.toml`
- Justify new dependencies and check licenses (AGPL-3.0 compatible)
- Run security checks: `cargo audit` (if available)
