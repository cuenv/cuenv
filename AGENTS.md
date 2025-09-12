# Nix Flake Development Environment

This project uses a Nix flake for development. To run commands, use:

`nix develop --command cargo --version`

Or enter the development shell with:
`nix develop`

## CRITICAL: Pre-Push Checklist

**NEVER push code without running these checks:**

1. **Run clippy with strict warnings** - MUST pass with no warnings:
   ```bash
   nix develop --command cargo clippy -- -D warnings
   ```

2. **Run treefmt** - MUST format all files:
   ```bash
   nix develop --command treefmt
   ```

3. **Run tests** - MUST pass all tests:
   ```bash
   nix develop --command cargo test
   ```

4. **Build the project** - MUST compile successfully:
   ```bash
   nix develop --command cargo build
   ```

If ANY of these checks fail, DO NOT push. Fix the issues first.

## Development Workflow

### Linting and Code Quality

This project uses Clippy for Rust linting. To run clippy with strict warnings:

```bash
# Run clippy with strict warnings
nix develop --command cargo clippy -- -D warnings
```

### Running Tests

```bash
# Run library tests
nix develop --command cargo test --lib
```

### Code Formatting

```bash
nix develop --command cargo fmt
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
