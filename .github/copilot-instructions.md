# cuenv - CUE-Powered Environment Management & Build Toolchain

**ALWAYS follow these instructions first and fallback to search or bash commands only when you encounter unexpected information that does not match the info here.**

cuenv is a Rust + Go FFI application that provides type-safe environment management and task orchestration using CUE configuration language. It consists of a core CUE evaluation engine (cuengine), shared utilities (core), and CLI interface (cuenv).

## Best Tasks for Copilot Agent

The following types of tasks are well-suited for Copilot agent:

- **Bug fixes**: Clear, well-defined bugs with reproduction steps
- **Feature implementation**: Incremental features with clear acceptance criteria
- **Test coverage**: Adding or improving unit and integration tests
- **Documentation**: Updating or creating documentation for code changes
- **Code refactoring**: Improving code quality while maintaining functionality
- **Technical debt**: Addressing linting issues, upgrading dependencies, etc.

When assigning tasks, ensure they:
- Have clear acceptance criteria
- Are well-scoped (not too broad)
- Indicate which files or areas will be affected
- Include any relevant context from previous discussions

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

### Code Conventions

**IMPORTANT coding practices:**

- **Event System**: Use `cuenv_events` emit macros for all output instead of direct console output (print!, println!, eprintln!)
- **Linting**: The workspace enforces `unsafe_code = "warn"` and `missing_docs = "warn"` 
- **Clippy**: All clippy warnings are treated as errors, including pedantic checks
- **Console Output**: Direct `print_stdout` and `print_stderr` are forbidden - use the event system instead
- **Documentation**: All public APIs must have documentation
- **Error Handling**: Use `thiserror` for error types and `miette` for rich error reporting
- **Edition**: Project uses Rust 2024 edition with MSRV 1.85.0

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
cargo run -- env print --path examples/env-basic --package examples

# Test JSON output format
cargo run -- env print --path examples/env-basic --package examples --output-format json

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
│   ├── core/              # Shared types, errors, validation (cuenv-core)
│   ├── cuenv/             # CLI interface with TUI support (main binary)
│   ├── events/            # Event system for tracing and diagnostics
│   ├── workspaces/        # Workspace detection and parsing
│   ├── dagger/            # Dagger integration
│   ├── ci/                # CI/CD automation utilities
│   ├── release/           # Release automation
│   └── schema-validator/  # CUE schema validation
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
cargo run -- env print --path examples/env-basic --package examples
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
