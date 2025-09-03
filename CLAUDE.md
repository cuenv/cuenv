# Devenv + Nix

No tooling is available in this project, so all commands must be prefixed and run as such:

`devenv shell -- cargo --version`

## Development Workflow

### Linting and Code Quality

This project uses Clippy for Rust linting. To run clippy with strict warnings:

```bash
# Using devenv (preferred)
devenv shell -- cargo clippy -- -D warnings

# Or using nix-shell directly
nix-shell -p gcc --run "cargo clippy -- -D warnings"
```

### Running Tests

```bash
# Run library tests
devenv shell -- cargo test --lib

# Or using nix-shell
nix-shell -p gcc --run "cargo test --lib"
```

### Code Formatting

```bash
devenv shell -- cargo fmt
```

## Recent Changes

### PR #8 - Bridge Version Diagnostics

**Latest Updates (Sept 2025):**

- ✅ Fixed all clippy warnings and linting issues
- ✅ Improved code quality with modern Rust idioms (let...else syntax, if let expressions)
- ✅ Added proper clippy allow attributes with justifications for FFI complexity
- ✅ All tests passing (25 tests in cuengine, 15 tests in cuenv-core)
- ✅ Code compiles cleanly with `clippy -D warnings`

**Key Improvements:**

- Enhanced error handling and JSON response parsing between Go CUE evaluator and Rust via FFI
- Added missing `Serialize` import alongside `Deserialize` for consistency
- Proper JSON marshaling error handling in Go bridge
- Defined error code constants on both Go and Rust sides to prevent desync
- Comprehensive test coverage for bridge version functionality and error handling
- Improved memory management and FFI safety

**Technical Details:**

- Bridge version diagnostics functionality fully implemented
- FFI wrapper with proper RAII memory management
- Structured error responses with typed error codes
- Version compatibility checking between Rust and Go sides
- Extensive test coverage including edge cases and error conditions
