---
title: Contributing
description: Guide for contributing to cuenv
---

We welcome contributions to cuenv! This guide will help you get started with contributing code, documentation, and ideas.

## Getting Started

### Prerequisites

- Rust 1.70+ (install via [rustup](https://rustup.rs/))
- Git for version control
- Nix (optional, for reproducible development environment)
- Go 1.21+ (for CUE engine development)

### Development Setup

```bash
# Clone the repository
git clone https://github.com/cuenv/cuenv.git
cd cuenv

# Set up development environment
nix develop  # or direnv allow

# Build the project
cargo build

# Run tests
cargo test --workspace
```

## Code Contributions

### Finding Issues to Work On

- Check the [issue tracker](https://github.com/cuenv/cuenv/issues)
- Look for issues labeled `good first issue` or `help wanted`
- Join [discussions](https://github.com/cuenv/cuenv/discussions) to propose new features

### Development Workflow

1. **Fork and Clone**

```bash
# Fork on GitHub, then clone your fork
git clone https://github.com/YOUR_USERNAME/cuenv.git
cd cuenv
git remote add upstream https://github.com/cuenv/cuenv.git
```

2. **Create a Branch**

```bash
git checkout -b feature/my-new-feature
```

3. **Make Changes**

- Follow the coding standards below
- Write tests for new functionality
- Update documentation as needed

4. **Test Your Changes**

```bash
# Run all tests
cargo test --workspace

# Run specific crate tests
cargo test -p cuengine
cargo test -p cuenv-core
cargo test -p cuenv-cli

# Run linting
cargo clippy -- -D warnings

# Format code
cargo fmt
```

5. **Commit and Push**

```bash
git add .
git commit -m "feat: add new feature description"
git push origin feature/my-new-feature
```

6. **Create Pull Request**

- Open a PR on GitHub
- Fill out the PR template
- Link any related issues

### Coding Standards

#### Rust Code Style

- Follow standard Rust formatting (`cargo fmt`)
- Use `cargo clippy` for linting
- Document public APIs with rustdoc comments
- Write unit tests for all public functions

````rust
use cuengine::evaluate_cue_package_typed;
use cuenv_core::manifest::Project;
use std::path::Path;

/// Evaluates the `cuenv` package inside `dir` and returns the typed manifest.
///
/// # Arguments
///
/// * `dir` - Directory containing your `env.cue`
///
/// # Errors
///
/// Returns any evaluation or deserialization error emitted by the Go bridge.
///
/// # Examples
///
/// ```rust
/// # use cuengine::evaluate_cue_package;
/// # use std::path::Path;
/// let json = evaluate_cue_package(Path::new("./config"), "cuenv")?;
/// assert!(json.contains("env"));
/// # Ok::<_, cuengine::CueEngineError>(())
/// ```
pub fn load_manifest(dir: &Path) -> cuengine::Result<Project> {
    evaluate_cue_package_typed(dir, "cuenv")
}
````

#### Error Handling

Use structured error types with `thiserror`:

```rust
#[derive(Debug, thiserror::Error)]
pub enum MyError {
    #[error("Configuration error: {message}")]
    Config { message: String },

    #[error("IO error")]
    Io(#[from] std::io::Error),
}
```

#### Testing

Write comprehensive tests using Rust's built-in test framework:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use cuengine::evaluate_cue_package;
    use std::path::Path;

    #[test]
    fn evaluator_runs() {
        let result = evaluate_cue_package(Path::new("."), "cuenv");
        assert!(result.is_ok());
    }
}
```

### Project Structure

```
cuenv/
├── crates/
│   ├── cuengine/          # CUE evaluation engine
│   ├── cuenv-core/        # Core library
│   ├── cuenv/             # CLI application
│   ├── cuenv-dagger/      # Dagger backend
│   ├── cuenv-events/      # Event system
│   ├── cuenv-ci/          # CI support
│   └── cuenv-release/     # Release management
├── integrations/          # Editor integrations (VSCode)
├── docs/                  # Documentation source
├── _examples/             # Example configurations
├── schema/                # CUE schemas
└── _tests/                # Integration tests
```

### Adding New Features

When adding new features:

1. **Design First**: Discuss the design in an issue or discussion
2. **Start Small**: Implement the minimal viable version
3. **Test Thoroughly**: Add unit and integration tests
4. **Document**: Update relevant documentation
5. **Performance**: Consider performance implications

### Working with FFI

For changes to the CUE engine FFI:

1. **Go Side**: Update `crates/cuengine/bridge.go`
2. **Rust Side**: Update `crates/cuengine/src/lib.rs`
3. **Test Both**: Ensure both Go and Rust tests pass
4. **Memory Safety**: Verify proper memory management

## Documentation Contributions

### Documentation Structure

Documentation uses [Astro](https://astro.build/) with [Starlight](https://starlight.astro.build/):

- Source files in `docs/src/content/docs/`
- Configuration in `docs/astro.config.mjs`
- Build with `bun run build`

### Writing Guidelines

- Use clear, concise language
- Include code examples for APIs
- Add cross-references between related topics
- Test all code examples

### Building Documentation

```bash
cd docs

# Install dependencies
bun install

# Development server with hot reload
bun run dev

# Build for production
bun run build

# Preview production build
bun run preview
```

## Bug Reports

### Before Reporting

- Check if the issue already exists
- Try to reproduce with the latest version
- Create a minimal reproduction case

### Bug Report Template

When reporting bugs, include:

- **Environment**: OS, Rust version, cuenv version
- **Steps to Reproduce**: Clear, numbered steps
- **Expected Behavior**: What should happen
- **Actual Behavior**: What actually happens
- **Reproduction Case**: Minimal code/config to reproduce

### Example Bug Report

````markdown
## Bug Description

CUE evaluation fails with circular reference error

## Environment

- OS: Ubuntu 20.04
- Rust: 1.75.0
- cuenv: 0.1.1

## Steps to Reproduce

1. Create file `config.cue` with content:
   ```cue
   a: b
   b: a
   ```
````

2. Run `cuenv env print`

## Expected Behavior

Should report circular reference error clearly

## Actual Behavior

Crashes with panic: "stack overflow"

## Reproduction Case

[Link to minimal repo or paste configuration]

```

## Feature Requests

### Before Requesting

* Check if a similar feature exists or is planned
* Consider if it fits with cuenv's goals
* Think about implementation complexity

### Feature Request Template

Include:

* **Use Case**: Why is this needed?
* **Proposal**: How should it work?
* **Alternatives**: What other solutions exist?
* **Implementation**: Any implementation ideas?

## Release Process

### Versioning

cuenv follows Semantic Versioning:

* **Major** (1.0.0): Breaking changes
* **Minor** (0.1.x): Feature additions
* **Patch** (0.1.1): Bug fixes

### Release Checklist

For maintainers preparing releases:

1. Update version numbers in `Cargo.toml` files
2. Update `CHANGELOG.md`
3. Run full test suite
4. Build documentation
5. Tag release
6. Publish to crates.io

## Community Guidelines

### Code of Conduct

We follow the [Contributor Covenant](https://www.contributor-covenant.org/) code of conduct. Be respectful and inclusive.

### Communication

* **GitHub Issues**: Bug reports and feature requests
* **GitHub Discussions**: General questions and design discussions
* **Pull Requests**: Code contributions and reviews

### Getting Help

* Read the documentation first
* Search existing issues and discussions
* Ask specific questions with context
* Provide minimal reproduction cases

## Recognition

Contributors will be:

* Listed in `CONTRIBUTORS.md`
* Mentioned in release notes for significant contributions
* Invited to join the cuenv organization (for regular contributors)

Thank you for contributing to cuenv!
```
