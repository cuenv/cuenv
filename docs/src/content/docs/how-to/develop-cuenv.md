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
jj git clone https://github.com/cuenv/cuenv
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
jj git clone https://github.com/cuenv/cuenv
cd cuenv
cuenv task build
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
│   └── cuenv-release/      # Release management
├── schema/                 # CUE schema definitions
├── integrations/           # Editor integrations (VSCode)
├── examples/               # Example configurations
├── docs/                   # Documentation (Astro/Starlight)
├── tests/                  # Integration tests
└── flake.nix              # Nix development environment
```

## Project automation (this repo)

This repository defines its own tasks in `env.cue`. Use these as the canonical way to format, lint, test, and build:

```bash
cuenv task fmt.check
cuenv task lint
cuenv task test.unit
cuenv task build
cuenv task coverage
```

If you want to apply formatting changes:

```bash
cuenv task fmt.fix
```

## Running cuenv from source

```bash
# List tasks in this repo's env.cue
cargo run -p cuenv -- task

# Evaluate a fixture
cargo run -p cuenv -- env print --path ./examples/env-basic
```

## Working on specific crates

You can still work at the Cargo level when you need to:

```bash
cargo build -p cuengine
cargo test -p cuenv-core
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

Releases are managed by cuenv's native release tooling:

### Automated Workflow

1. Write code with [Conventional Commits](https://www.conventionalcommits.org/) (`feat:`, `fix:`, `feat!:`)
2. Push to main - CI automatically creates a release PR with version bumps
3. Review and merge the release PR
4. Create a GitHub Release - CI publishes crates and builds artifacts

### Version Bumping

Versions are determined from commit messages:

- `feat:` - Minor version bump (e.g., 0.8.0 → 0.9.0)
- `fix:` or `perf:` - Patch version bump (e.g., 0.8.0 → 0.8.1)
- `feat!:` or `BREAKING CHANGE:` - Major version bump (e.g., 0.8.0 → 1.0.0)

### Manual Workflow

For more control, you can manage releases manually:

```bash
# Create a changeset for specific packages
cuenv changeset add --packages cuenv-core:minor --summary "Add new feature"

# Or generate from conventional commits
cuenv changeset from-commits --since 0.9.0

# Check pending changesets
cuenv changeset status

# Preview version changes
cuenv release version --dry-run

# Apply version changes (updates Cargo.toml, generates changelog)
cuenv release version

# See publish order
cuenv release publish --dry-run
```

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

- [Contributing Guide](/how-to/contribute/) - Contribution workflow
- [Architecture](/explanation/architecture/) - System design
- [API Reference](/reference/rust-api/) - Public APIs
