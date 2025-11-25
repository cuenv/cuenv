# Nix Flake Development Environment

This project uses a Nix flake for development. To run commands, use:

`nix develop --command cargo --version`

Or enter the development shell with:
`nix develop`

## cuenv Task Execution Policy

- Always run project automation via `cuenv task <name>` (e.g., `cuenv task fmt.check`, `cuenv task lint`, `cuenv task test.unit`, `cuenv task coverage`, `cuenv task build`).
- **Never** wrap these invocations in `nix develop`—the cuenv hooks automatically hydrate the required Nix environment.
- Alternative command sequences (manual `nix develop`, direct `cargo`, etc.) are not accepted for validation; stick to the documented `cuenv task` flow.

## Version Control Policy

- Use [`jj`](https://github.com/martinvonz/jj) for every version-control task in this repository.
- Run `jj status`, `jj diff`, `jj log`, etc., instead of their Git equivalents.
- When you need to interact with remotes, use `jj git push`/`jj git fetch` rather than any `git` commands.
- Direct `git` invocations are prohibited; `jj` already manages the underlying Git storage for you.

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
   nix develop --command cargo nextest run
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
# Run all tests with nextest
nix develop --command cargo nextest run

# Run library tests only
nix develop --command cargo nextest run --lib

# Run BDD tests
nix develop --command cargo test --test bdd
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
