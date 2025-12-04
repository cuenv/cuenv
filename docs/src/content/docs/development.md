---
title: Development
description: Development workflow and best practices for cuenv
---

This guide covers the development workflow for contributing to cuenv itself.

## Prerequisites

- **Rust** 1.70+ via [rustup](https://rustup.rs/)
- **Go** 1.21+ for the CUE evaluation bridge
- **Nix** (recommended) for reproducible development environment
- **Git** for version control

## Development Setup

### Using Nix (Recommended)

The easiest way to get a complete development environment:

```bash
# Clone the repository
git clone https://github.com/cuenv/cuenv.git
cd cuenv

# Enter the development shell
nix develop

# Or use direnv for automatic loading
direnv allow
```

The Nix shell provides:

- Rust toolchain with clippy, rustfmt, and rust-analyzer
- Go for the CUE bridge
- All required system dependencies
- Development tools (treefmt, cargo-nextest, etc.)

### Manual Setup

If not using Nix:

```bash
# Install Rust
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup component add clippy rustfmt

# Install Go
# Download from https://go.dev/dl/

# Clone and build
git clone https://github.com/cuenv/cuenv.git
cd cuenv
cargo build
```

## Project Structure

```
cuenv/
├── crates/
│   ├── cuengine/           # CUE evaluation engine (Rust + Go FFI)
│   │   ├── src/            # Rust source
│   │   ├── bridge.go       # Go FFI bridge
│   │   └── tests/
│   ├── cuenv-core/         # Core types and utilities
│   ├── cuenv/              # CLI application
│   ├── cuenv-dagger/       # Dagger backend integration
│   ├── cuenv-workspaces/   # Workspace management
│   ├── cuenv-events/       # Event system
│   ├── cuenv-ci/           # CI pipeline support
│   ├── cuenv-release/      # Release management
│   └── schema-validator/   # Schema validation
├── schema/                 # CUE schema definitions
├── integrations/           # Editor integrations (VSCode)
├── examples/               # Example configurations
├── docs/                   # Documentation (Astro/Starlight)
├── tests/                  # Integration tests
└── flake.nix              # Nix development environment
```

## Build Commands

### Basic Build

```bash
# Debug build
cargo build

# Release build
cargo build --release

# Build specific crate
cargo build -p cuengine
```

### Running cuenv

```bash
# Run from source
cargo run -- task

# Run with arguments
cargo run -- env print --path ./examples/env-basic
```

## Testing

### Unit Tests

```bash
# Run all tests
cargo test --workspace

# Run tests for specific crate
cargo test -p cuengine
cargo test -p cuenv-core

# Run with nextest (faster, better output)
cargo nextest run

# Run specific test
cargo test test_basic_evaluation
```

### Integration Tests

```bash
# Run BDD tests
cargo test --test bdd

# Run example validation
cargo test --test examples
```

### Test Coverage

```bash
# Generate coverage report
cargo llvm-cov

# HTML report
cargo llvm-cov --html
```

## Code Quality

### Formatting

```bash
# Check formatting
cargo fmt --check

# Fix formatting
cargo fmt

# Format all files (Rust, CUE, etc.) with treefmt
treefmt
```

### Linting

```bash
# Run clippy
cargo clippy

# Run with strict warnings (required for CI)
cargo clippy -- -D warnings
```

### Pre-Commit Checklist

Before pushing, always run:

```bash
# 1. Format code
treefmt

# 2. Run clippy with strict warnings
cargo clippy -- -D warnings

# 3. Run all tests
cargo nextest run

# 4. Build successfully
cargo build
```

## Working with FFI

The cuengine crate uses FFI to call Go from Rust.

### Building the Go Bridge

The Go bridge is built automatically during `cargo build` via a build script.

**Manual build:**

```bash
cd crates/cuengine
go build -buildmode=c-archive -o libcue_bridge.a bridge.go
```

### FFI Development Tips

1. **Memory Safety**: All C strings from Go must be freed with `cue_free_string`
2. **Error Handling**: The bridge uses a JSON envelope for structured errors
3. **Thread Safety**: `CStringPtr` is intentionally `!Send + !Sync`
4. **Testing**: Test both Go and Rust sides when modifying the bridge

### Modifying the Bridge

When changing the FFI interface:

1. Update `crates/cuengine/bridge.go`
2. Update `crates/cuengine/src/lib.rs`
3. Ensure error codes match on both sides
4. Add tests for new functionality

## Documentation

### Building Docs

```bash
cd docs

# Install dependencies
bun install

# Development server
bun run dev

# Build for production
bun run build
```

### Writing Documentation

- Documentation uses [Astro](https://astro.build/) with [Starlight](https://starlight.astro.build/)
- Source files in `docs/src/content/docs/`
- Use Markdown with MDX support
- Test code examples before documenting

## Debugging

### Verbose Logging

```bash
# Enable debug logging
RUST_LOG=debug cargo run -- task build

# Trace level for FFI debugging
RUST_LOG=cuengine=trace cargo run -- env print
```

### GDB/LLDB

```bash
# Build with debug symbols
cargo build

# Debug with lldb
lldb target/debug/cuenv
```

### Common Issues

**Go bridge build fails:**

```bash
# Ensure Go is in PATH
go version

# Clean and rebuild
cargo clean
cargo build
```

**FFI panics:**

- Check that Go code doesn't panic without recovery
- Verify memory isn't double-freed
- Enable trace logging to see FFI calls

## Release Process

Releases are managed by [release-plz](https://release-plz.ieni.dev/):

1. Commits follow [Conventional Commits](https://www.conventionalcommits.org/)
2. PRs trigger changelog generation
3. Merging updates versions and publishes crates

### Version Bumping

Versions are automatically determined from commit messages:

- `feat:` - Minor version bump
- `fix:` - Patch version bump
- `feat!:` or `BREAKING CHANGE:` - Major version bump

## IDE Setup

### VS Code

Recommended extensions:

- rust-analyzer
- Even Better TOML
- cuelang.cue

**settings.json:**

```json
{
  "rust-analyzer.cargo.features": "all",
  "rust-analyzer.check.command": "clippy"
}
```

### IntelliJ/CLion

- Install Rust plugin
- Configure Rust toolchain from Nix shell if using Nix

### Neovim

With nvim-lspconfig:

```lua
require('lspconfig').rust_analyzer.setup{
  settings = {
    ["rust-analyzer"] = {
      cargo = { features = "all" },
      check = { command = "clippy" }
    }
  }
}
```

## See Also

- [Contributing Guide](/contributing/) - Contribution workflow
- [Architecture](/architecture/) - System design
- [API Reference](/api-reference/) - Public APIs
