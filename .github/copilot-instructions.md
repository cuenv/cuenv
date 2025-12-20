# cuenv - CUE-Powered Environment Management & Build Toolchain

**ALWAYS follow these instructions first and fallback to search or bash commands only when you encounter unexpected information that does not match the info here.**

cuenv is a Rust + Go FFI application that provides type-safe environment management and task orchestration using CUE configuration language. It consists of a core CUE evaluation engine (cuengine), shared utilities (cuenv-core), and CLI interface (cuenv-cli).

## Working Effectively

### Bootstrap and Build

Run these commands in order to get a working development environment:

**CRITICAL BUILD TIMING:** Build takes 1.5-2 minutes for debug, 45+ seconds for release. Tests take 45-60 seconds. **NEVER CANCEL** these operations. Set timeouts to 120+ minutes for builds, 60+ minutes for tests.

```bash
# Build the entire workspace (NEVER CANCEL - takes 90+ seconds)
cargo build --workspace --all-features

# Build release version (NEVER CANCEL - takes 45+ seconds)
cargo build --release --workspace

# Run all tests (NEVER CANCEL - takes 45-60 seconds)
cargo test --workspace

# Run tests with library-only (faster, 30+ seconds)
cargo test --lib --workspace
```

### Code Quality and Formatting

```bash
# Format code (required before commits)
cargo fmt

# Check formatting without changes
cargo fmt --check

# Run clippy linting (takes 15-20 seconds)
cargo clippy --workspace --all-targets --all-features -- -D warnings

# Format Go code in cuengine directory
cd crates/cuengine && gofmt -w .
```

### Development Workflow with Nix Flake

This project uses a Nix flake for reproducible development environments:

```bash
# Enter the development shell
nix develop

# Or run commands directly:
nix develop --command cargo build --workspace --all-features
nix develop --command cargo test --workspace
nix develop --command cargo clippy --workspace --all-targets --all-features -- -D warnings
nix develop --command treefmt --fail-on-change  # Format all files
```

**If Nix is NOT available:** Use standard cargo commands directly (as shown above).

## Validation Scenarios

### Always Test These Scenarios After Making Changes:

1. **Build validation**: Ensure both debug and release builds succeed
2. **Basic functionality**: Test the CLI with example CUE files
3. **Error handling**: Test with invalid inputs to verify error messages

```bash
# Test basic CLI functionality
cargo run -- version
cargo run -- env print --path examples/env-basic --package _examples

# Test JSON output format
cargo run -- env print --path examples/env-basic --package _examples --output-format json

# Test error handling with invalid path
cargo run -- env print --path /nonexistent
```

Expected outputs:

- Version command shows version info with correlation ID
- env print shows environment variables in KEY=VALUE format
- JSON format outputs valid JSON structure
- Invalid path shows helpful error message with exit code 3

## Timing Expectations and Timeouts

**CRITICAL: NEVER CANCEL long-running operations. Always use appropriate timeouts:**

| Operation                 | Expected Time | Minimum Timeout |
| ------------------------- | ------------- | --------------- |
| `cargo build --workspace` | 90 seconds    | 180 seconds     |
| `cargo build --release`   | 45 seconds    | 120 seconds     |
| `cargo test --workspace`  | 50 seconds    | 120 seconds     |
| `cargo clippy`            | 18 seconds    | 60 seconds      |
| `cargo bench`             | 60+ seconds   | 300 seconds     |

### Build Process Details

- **Debug build**: Downloads dependencies first (~30s), then compiles (~60s)
- **Release build**: Longer optimization phase, but fewer total dependencies
- **Tests**: Runs 75+ tests across all crates (cuengine: 25, cuenv-cli: 50+, cuenv-core: 17)
- **Go FFI tests**: Require CGO and may fail in some CI environments

## Repository Structure

```
cuenv/
├── crates/
│   ├── cuengine/          # Core CUE evaluation engine (Rust + Go FFI)
│   │   ├── bridge.go      # Go bridge for CUE language integration
│   │   ├── build.rs       # Rust build script for Go compilation
│   │   └── src/           # Rust FFI wrapper and caching
│   ├── cuenv-core/        # Shared types, errors, validation
│   └── cuenv-cli/         # CLI interface with TUI support
├── examples/
│   └── env-basic/         # Example CUE configuration files
├── .github/workflows/     # CI/CD configuration
└── flake.nix             # Nix flake configuration
```

## Common Tasks

### Working with CUE Files

cuenv evaluates CUE configuration files to extract environment variables:

```bash
# List what's in the examples directory
ls examples/env-basic/

# View example CUE file
cat examples/env-basic/env.cue

# Test with the example
cargo run -- env print --path examples/env-basic --package _examples
```

### Testing Changes to Core Engine

```bash
# Test only the core engine
cd crates/cuengine && cargo test

# Run integration tests
cargo test --test integration_tests

# Test FFI edge cases
cargo test --test ffi_edge_cases
```

### Performance Testing

```bash
# Run benchmarks (NEVER CANCEL - takes 60+ seconds)
cargo bench --workspace --no-fail-fast
```

## CI/CD Integration

The project uses GitHub Actions with these key jobs:

- **lint-and-format**: treefmt and clippy checks
- **test-suite**: Tests on Ubuntu + macOS with Rust stable + MSRV (1.85.0)
- **supply-chain-security**: cargo-audit and cargo-deny checks
- **coverage**: Code coverage with cargo-llvm-cov
- **benchmarks**: Performance regression testing

### Always run before committing:

```bash
cargo fmt
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
```

## Troubleshooting

### Common Issues:

1. **Nix not available**: Use standard cargo commands instead
2. **Go FFI tests fail**: This is expected in some environments without CGO
3. **cargo-audit/cargo-deny not found**: These are CI-only tools, skip locally
4. **Build appears frozen**: Builds can take 90+ seconds, especially first run

### Build Failures:

- Check Rust edition compatibility (requires 2024 edition, MSRV 1.85.0)
- Ensure Go is available for cuengine FFI bridge compilation
- Clear target directory: `rm -rf target/` and rebuild

### FFI Bridge Issues:

The Go bridge in `crates/cuengine/` provides CUE language evaluation:

- Requires Go 1.21+ and CGO enabled
- Uses build.rs to compile Go code into static library
- Memory management handled via Rust FFI wrappers

## Key Files to Monitor

When making changes, always check these files:

- `Cargo.toml` (workspace configuration)
- `crates/cuengine/bridge.go` (Go FFI implementation)
- `crates/cuengine/src/lib.rs` (Rust FFI wrapper)
- `examples/env-basic/env.cue` (test CUE configuration)

## Security and Dependencies

The project uses cargo-deny for dependency checking:

- AGPL-3.0-or-later license (same as project)
- Allows MIT, Apache-2.0, BSD licenses
- Monitors security advisories
- Run `cargo audit` if available for vulnerability scanning

---

**Remember: This is an alpha-stage project focused on CUE evaluation and environment management. The CLI interface is in active development, but the core evaluation engine is production-ready.**

## Rust Coding Standards

This section provides Rust-specific development guidelines for the cuenv codebase. Following these patterns ensures consistency, maintainability, and adherence to project conventions.

### Rust Edition & Toolchain

- **Rust Edition 2024** (MSRV 1.85.0)
- Uses `treefmt` with `rustfmt --edition 2024` for formatting
- Go FFI bridge via `cuengine` crate (requires CGO)

### Error Handling

Error handling in cuenv follows a structured, diagnostic-focused approach:

#### Core Principles

1. **Use `thiserror` for structured error enums in library code**
2. **Use `miette::Diagnostic` alongside `thiserror::Error` for rich CLI errors**
3. **NO `unwrap()` or `expect()` in production code** - always use `?` propagation
4. **Constructor methods with `#[must_use]` for error creation**

#### Error Categories & Exit Codes

- `Config` errors: exit code 2
- `Eval` errors: exit code 3
- `Other` errors: exit code 3

#### Example Error Pattern

```rust
use thiserror::Error;
use miette::Diagnostic;

#[derive(Error, Debug, Diagnostic)]
pub enum MyError {
    #[error("Configuration error: {message}")]
    #[diagnostic(
        code(cuenv::config::invalid),
        help("Check your configuration file for syntax errors")
    )]
    Configuration {
        #[source_code]
        src: String,
        #[label("invalid configuration")]
        span: Option<SourceSpan>,
        message: String,
    },

    #[error("Validation failed: {message}")]
    #[diagnostic(code(cuenv::validation::failed))]
    Validation {
        message: String,
        #[help]
        help: Option<String>,
    },
}

impl MyError {
    /// Create a configuration error
    #[must_use]
    pub fn configuration(message: impl Into<String>) -> Self {
        Self::Configuration {
            src: String::new(),
            span: None,
            message: message.into(),
        }
    }
}
```

### Workspace Lints

All code must adhere to workspace-level lints defined in `Cargo.toml`:

```toml
[workspace.lints.rust]
unsafe_code = "warn"
missing_docs = "warn"

[workspace.lints.clippy]
all = "warn"
pedantic = "warn"
print_stdout = "warn"  # Use cuenv_events macros instead
print_stderr = "warn"
```

**Critical**: Never use `println!` or `eprintln!` - use `cuenv_events` macros instead (see Event-Driven Output section).

### Serde Conventions

Follow these patterns for serialization:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Config {
    /// Optional field - omit from JSON when None
    #[serde(skip_serializing_if = "Option::is_none")]
    pub optional_field: Option<String>,
    
    /// Collection with default empty value
    #[serde(default)]
    pub items: Vec<String>,
    
    /// Field with custom serialization name
    #[serde(rename = "specialName")]
    pub special: String,
}
```

**Common rename conventions**:
- `camelCase` - Most common for API types
- `kebab-case` - Used for CLI/config file formats
- `lowercase` - Used for simple enums

### Code Patterns

#### 1. Trait-Driven Design

Implement standard traits for all public types:

```rust
// Always implement these when appropriate
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct MyType {
    // ...
}

impl Default for MyType {
    fn default() -> Self {
        Self::new()
    }
}

impl Display for MyType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MyType({})", self.field)
    }
}

// Prefer `impl Trait` over verbose generics
pub fn process_items(items: impl Iterator<Item = String>) -> Vec<String> {
    items.collect()
}

// Ensure Send + Sync for types used across threads
pub struct ThreadSafeType {
    // ...
}

// Compiler will verify Send + Sync automatically for most types
```

#### 2. Functional Programming

Prefer functional patterns over imperative code:

```rust
// ✅ GOOD: Iterator chains
let result: Vec<_> = items
    .iter()
    .filter(|x| x.is_valid())
    .map(|x| x.process())
    .collect();

// ❌ BAD: Imperative loops
let mut result = Vec::new();
for item in items.iter() {
    if item.is_valid() {
        result.push(item.process());
    }
}

// ✅ GOOD: Expressions over statements
let value = if condition {
    calculate_a()
} else {
    calculate_b()
};

// ✅ GOOD: Combinator patterns for Option/Result
let result = some_option
    .ok_or_else(|| Error::missing("value"))
    .and_then(|x| validate(x))
    .map(|x| transform(x))?;
```

#### 3. Newtype Pattern

Wrap primitives for domain types with validation:

```rust
use std::path::{Path, PathBuf};

/// Validated directory path that must exist
#[derive(Debug, Clone)]
pub struct PackageDir(PathBuf);

impl TryFrom<&Path> for PackageDir {
    type Error = Error;

    fn try_from(path: &Path) -> Result<Self, Self::Error> {
        if !path.exists() {
            return Err(Error::validation(format!("Path does not exist: {}", path.display())));
        }
        if !path.is_dir() {
            return Err(Error::validation(format!("Path is not a directory: {}", path.display())));
        }
        Ok(Self(path.to_path_buf()))
    }
}

impl AsRef<Path> for PackageDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}
```

#### 4. Builder Pattern

For complex structs, use the builder pattern with `#[must_use]`:

```rust
pub struct Config {
    pub host: String,
    pub port: u16,
    pub timeout: Duration,
}

impl Config {
    #[must_use]
    pub fn builder() -> ConfigBuilder {
        ConfigBuilder::default()
    }
}

#[derive(Default)]
pub struct ConfigBuilder {
    host: Option<String>,
    port: Option<u16>,
    timeout: Option<Duration>,
}

impl ConfigBuilder {
    #[must_use]
    pub fn host(mut self, host: impl Into<String>) -> Self {
        self.host = Some(host.into());
        self
    }

    #[must_use]
    pub fn port(mut self, port: u16) -> Self {
        self.port = Some(port);
        self
    }

    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    pub fn build(self) -> Result<Config, Error> {
        Ok(Config {
            host: self.host.ok_or_else(|| Error::validation("host is required"))?,
            port: self.port.unwrap_or(8080),
            timeout: self.timeout.unwrap_or(Duration::from_secs(30)),
        })
    }
}
```

#### 5. Event-Driven Output

**NEVER** use `println!` or `eprintln!` - all output must go through `cuenv_events` macros:

```rust
use cuenv_events::{emit_task_started, emit_task_completed, emit_info};

// ✅ GOOD: Using event macros
emit_task_started!("build", "cargo build", true);
emit_info!("Processing file: {}", file_path);

// ❌ BAD: Direct console output (will fail clippy)
println!("Processing file: {}", file_path);
eprintln!("Error: {}", error);
```

Available event macros:
- `emit_task_started!` - Task execution started
- `emit_task_completed!` - Task execution completed
- `emit_task_cache_hit!` - Task cache hit
- `emit_task_cache_miss!` - Task cache miss
- `emit_info!` - Informational message
- `emit_warning!` - Warning message
- `emit_error!` - Error message

#### 6. FFI Safety

When working with FFI (especially in `cuengine` crate):

```rust
use std::ffi::{CStr, CString};

/// RAII wrapper for C string pointers
pub struct CStringPtr(*mut std::os::raw::c_char);

impl CStringPtr {
    /// Create a new wrapper from a raw pointer
    ///
    /// # Safety
    ///
    /// The pointer must be valid, non-null, and obtained from
    /// `CString::into_raw()` or equivalent. The caller transfers
    /// ownership to this wrapper.
    pub unsafe fn new(ptr: *mut std::os::raw::c_char) -> Self {
        Self(ptr)
    }

    /// Convert to Rust string slice
    ///
    /// # Safety
    ///
    /// The pointer must be valid and contain a valid UTF-8 C string.
    pub unsafe fn to_str(&self) -> Result<&str, std::str::Utf8Error> {
        CStr::from_ptr(self.0).to_str()
    }
}

impl Drop for CStringPtr {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: Pointer is valid and was created from CString::into_raw()
            unsafe {
                let _ = CString::from_raw(self.0);
            }
        }
    }
}
```

**FFI Safety Guidelines**:
1. Always use RAII wrappers for foreign memory
2. Document `// SAFETY:` comments for all `unsafe` blocks
3. Never expose raw pointers in public APIs
4. Validate all data crossing FFI boundary
5. Use `#[repr(C)]` for FFI structs

### Async Patterns

When writing async code:

```rust
use tokio::task::JoinSet;

/// Process items with bounded parallelism
pub async fn process_parallel(items: Vec<Item>, max_concurrent: usize) -> Result<Vec<Output>> {
    let mut set = JoinSet::new();
    let mut results = Vec::new();

    for item in items {
        // Limit concurrent tasks
        while set.len() >= max_concurrent {
            if let Some(result) = set.join_next().await {
                results.push(result??);
            }
        }

        set.spawn(async move {
            item.process().await
        });
    }

    // Collect remaining results
    while let Some(result) = set.join_next().await {
        results.push(result??);
    }

    Ok(results)
}

/// Provide sync alternatives when async not needed
pub fn process_sync(items: Vec<Item>) -> Result<Vec<Output>> {
    items.into_iter()
        .map(|item| item.process_sync())
        .collect()
}
```

### Testing Patterns

#### Test Organization

- Use Rust's built-in test framework for unit tests
- Place test helpers in `test_utils` module (in `src/test_utils.rs` with `#[cfg(test)]`)
- Integration tests go in `tests/` directory
- BDD tests use `cucumber` crate

#### Unit Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validation_success() {
        let input = "valid-package-name";
        let result = PackageName::try_from(input);
        assert!(result.is_ok());
        assert_eq!(result.unwrap().as_str(), input);
    }

    #[test]
    fn test_validation_failure() {
        let input = "invalid package name";
        let result = PackageName::try_from(input);
        assert!(result.is_err());
    }

    #[test]
    fn test_error_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<MyError>();
    }
}
```

#### Test Utilities Module

```rust
// src/test_utils.rs
#[cfg(test)]
pub mod test_utils {
    use super::*;

    /// Create a test task with dependencies
    pub fn create_task(name: &str, deps: Vec<&str>) -> Task {
        Task {
            command: format!("echo {}", name),
            depends_on: deps.into_iter().map(String::from).collect(),
            description: Some(format!("Test task {}", name)),
            ..Default::default()
        }
    }
}
```

#### Integration Tests

```rust
// tests/integration_tests.rs
use cuengine::evaluate_cue_package;
use tempfile::TempDir;

#[test]
fn test_evaluate_simple_package() {
    let temp_dir = TempDir::new().unwrap();
    let cue_content = r#"package test
env: {
    FOO: "bar"
}
"#;
    std::fs::write(temp_dir.path().join("env.cue"), cue_content).unwrap();

    let result = evaluate_cue_package(temp_dir.path(), "test");
    assert!(result.is_ok());
}
```

#### BDD Tests with Cucumber

```rust
// tests/bdd.rs
use cucumber::{World, given, when, then};

#[derive(Debug, World)]
#[world(init = Self::new)]
pub struct TestWorld {
    current_dir: PathBuf,
    last_output: String,
}

impl TestWorld {
    async fn new() -> Self {
        // Setup test world
    }
}

#[given("a cuenv project")]
async fn given_project(world: &mut TestWorld) {
    // Setup code
}

#[when(expr = "I run {string}")]
async fn when_run_command(world: &mut TestWorld, command: String) {
    // Execute command
}

#[then(expr = "the output contains {string}")]
async fn then_output_contains(world: &mut TestWorld, expected: String) {
    assert!(world.last_output.contains(&expected));
}
```

### Key Dependencies & Usage

#### Core Crates

- `thiserror` - Error definitions with `#[derive(Error)]`
- `miette` - Diagnostic errors with source code spans
- `serde` - Serialization with `#[derive(Serialize, Deserialize)]`
- `tracing` - Structured logging (NOT `log` crate)
- `tokio` - Async runtime with `#[tokio::main]` or `#[tokio::test]`
- `clap` - CLI parsing with `#[derive(Parser)]`
- `petgraph` - Task dependency graph operations
- `ratatui` + `crossterm` - TUI components

#### Testing Dependencies

- `tempfile` - Temporary directories for tests
- `proptest` - Property-based testing
- `criterion` - Benchmarking
- `cucumber` + `gherkin` - BDD testing
- `tokio-test` - Async test utilities

### Documentation

Follow Rust documentation conventions:

```rust
//! Module-level documentation
//!
//! This module provides X functionality for Y purpose.
//!
//! # Examples
//!
//! ```
//! use cuenv_core::MyType;
//!
//! let instance = MyType::new();
//! ```

/// Function documentation with examples
///
/// # Arguments
///
/// * `path` - The path to evaluate
/// * `package` - The CUE package name
///
/// # Errors
///
/// Returns `Error::Configuration` if the path does not exist.
/// Returns `Error::CueParse` if the CUE files are invalid.
///
/// # Examples
///
/// ```no_run
/// use cuenv::evaluate;
///
/// let result = evaluate("./config", "prod")?;
/// ```
pub fn evaluate(path: &Path, package: &str) -> Result<Config> {
    // Implementation
}
```

### License Compliance

- Project license: **AGPL-3.0-or-later**
- Use `cargo-deny` for dependency license checking
- Allowed licenses: MIT, Apache-2.0, BSD-*
- Run `cargo audit` for security advisories

### Common Pitfalls to Avoid

1. ❌ **Don't use `unwrap()` or `expect()` in production code** - always propagate errors with `?`
2. ❌ **Don't use `println!` or `eprintln!`** - use `cuenv_events` macros
3. ❌ **Don't ignore clippy warnings** - fix them or explicitly allow with justification
4. ❌ **Don't expose raw FFI pointers** - wrap in RAII types
5. ❌ **Don't write imperative loops** - prefer iterator chains
6. ❌ **Don't skip documentation** - all public items need doc comments
7. ❌ **Don't use blocking I/O in async contexts** - use `tokio::fs` and `tokio::io`
8. ❌ **Don't forget `#[must_use]` on builder methods** - ensures callers don't ignore return values

### Quick Reference

**Error Creation**:
```rust
Error::configuration("message")
Error::validation("message")
Error::ffi("function_name", "message")
```

**Event Emission**:
```rust
emit_task_started!("task", "command", false);
emit_info!("Message: {}", value);
```

**Validation Pattern**:
```rust
pub struct ValidatedType(String);

impl TryFrom<&str> for ValidatedType {
    type Error = Error;
    fn try_from(s: &str) -> Result<Self, Self::Error> {
        // validation logic
    }
}
```

**Builder Pattern**:
```rust
let config = Config::builder()
    .field1(value1)
    .field2(value2)
    .build()?;
```
