---
title: Contributing
description: Guide for contributing to cuenv
---

We welcome contributions to cuenv! This guide will help you get started with contributing code, documentation, and ideas.

## Getting Started

### Prerequisites

- Rust 1.90.0+ (install via [rustup](https://rustup.rs/))
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
# Run the final review/merge/release gate, not the default per-commit check.
# Release-only version bumps from an already-green main use the release-only checks below instead.
nix flake check -L --accept-flake-config

# Run specific crate tests
cuenv exec -- cargo test -p cuengine
cuenv exec -- cargo test -p cuenv-core
cuenv exec -- cargo test -p cuenv --lib

# Run generated workflow drift checks
cuenv sync ci --check

# Format code
cuenv fmt --fix
```

Use focused validation for isolated draft commits, then run the full Nix gate before requesting review or merging. Full flake checks are review/merge/release evidence and broad-risk safety nets, not the default proof for every draft commit. Release-only version bumps from an already-green `main` are the release exception: skip local test execution and the full root flake check, then verify version and lockfile consistency before tagging or publishing. If the change does not match a required full-flake trigger below, do not run the full root flake check before a draft commit; commit and push the isolated draft change with the focused validation recorded in the PR.

Before starting a full root flake check, identify the trigger that requires it. If no trigger applies, keep validation focused and record the focused gate instead.

Start with focused validation when the change is isolated and the focused gate directly covers the touched surface:

- Simple test extractions, mechanical refactors, test moves, or module splits with no behavior change: `cuenv fmt --fix`, `git diff --check`, and the focused crate/module test, or an app-local Nix test/clippy check when that is the local boundary. Add all-target clippy for the touched crate when the commit removes lint allowances or changes test-only dependencies.
- One-crate test-only Cargo manifest or lockfile changes, such as adding a dev-dependency used only by tests: `cuenv fmt --fix`, `git diff --check`, focused crate/module tests, all-target clippy for that crate, and a `Cargo.lock` review confirming the delta is limited to the test dependency.
- Docs, prompts, examples, repo-local agent skills, and agent-guidance text such as `AGENTS.md`: `cuenv task ci.schema-docs-check`.
- CLI behavior changes: focused Rust tests plus a direct CLI smoke test for the changed command.
- Sync-provider changes that do not alter generated workflow contracts: `cuenv sync ci --check` plus focused tests for the touched provider.

Do not run the full root flake check for exploratory review work, docs-only edits, prompt or agent-guidance text including `AGENTS.md`, repo-local skill-only edits, simple mechanical test extraction commits, test moves, behavior-preserving module splits, one-crate test-only dev-dependency changes, or tiny scoped commits while the PR is still draft and focused checks cover the touched surface.

Full root flake check is required before marking a PR ready for review, merging, release work that is not a release-only version bump from an already-green `main`, Nix/flake output/build/check wiring changes, CI/release behavior changes, generated workflow contract changes, broad cross-crate runtime changes, schema or CLI support changes that focused checks cannot fully cover, or Cargo manifest/lockfile changes that affect production dependencies, crate features, workspace membership, MSRV, published package metadata, or more than one crate.

If a change does not match one of those full-flake triggers, keep the check focused and record the focused validation in the PR.

Continuous integration runs the flake checks on `x86_64-linux` for every pull request and push to `main`. Pull requests run no macOS or ARM jobs. The `cuenv-darwin` workflow builds the `cuenv-clippy` and `cuenv-nextest` checks for `aarch64-darwin` on a macOS runner (the `darwin` tasks in `env.cue`) on every push to `main`, and `cuenv-arm` builds the `cuenv` package on an ARM Linux runner, so code that differs by platform is compiled and tested after it merges, not only when a release is built. If your change touches platform-specific code, run those workflows on your branch before merging:

```bash
gh workflow run cuenv-darwin.yml --ref <branch>
```

```bash
gh workflow run cuenv-arm.yml --ref <branch>
```

CI jobs get Nix store paths from a Namespace cache volume: each Linux runner mounts one at `/nix`, so a job starts with whatever an earlier job on that volume built, and anything missing comes from cache.nixos.org or is built. Volumes are per runner profile and the last job to finish writes it, so a job can still rebuild something another job built. macOS jobs do not mount a volume. To see whether a job started warm, check the Namespace cache step: it reports whether the cache paths were found and how much space they use. The repository no longer uses Hestia: its GitHub Actions cache was rate limited and evicting packs under parallel load, and once the volume was warm it served nothing.

The Nix audit uses the pinned RustSec advisory database offline and includes a
known-vulnerable fixture to verify that advisories are checked. It does not
check for yanked releases, because the sandbox has no registry index.

For performance changes, make the claim measurable. Add or update Criterion
coverage next to the hot path, keep a behavior regression test for the
optimized code path, and run the narrow benchmark slice before and after the
change. For example, task graph work can use
`cargo bench -p cuenv-task-graph --bench graph_benchmarks -- <group>` and event
rendering work can use
`cargo bench -p cuenv-events --bench json_renderer -- <group>`. Treat the
benchmark output as draft evidence; the full Nix gate is still only required
when one of the full-flake triggers applies or the PR is being readied for
review.

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

- Follow the [Google Rust Style Guide](https://google.github.io/styleguide/rustguide.html), applying API-design guidance in the context of cuenv's public contracts. Chromium- and Fuchsia-specific rules do not apply.
- Do not use boolean function parameters; group more than three non-receiver parameters in an options struct. Use named modes when a call needs to select behavior.
- Use the pinned Rust formatter with module-level import grouping, lowercase hex literals, and formatted macro matchers. The Nix checks cover the main workspace, Cuetty, and `tools/function-enum`.
- Use Clippy for linting. The root, Cuetty, and `tools/function-enum` Nix checks also enforce Clippy's wildcard-import and undocumented-unsafe-block lints for all targets, including crates that do not inherit the workspace lint settings.
- Document public APIs with rustdoc comments
- Write unit tests for all public functions

The parameter-style check parses first-party Rust files under `crates`, `apps`,
and `tools`, including tests and helpers. It checks direct boolean parameters
and counts non-receiver parameters. Trait implementations and foreign ABI
signatures are exempt when their signatures are fixed externally. The check
does not expand macros, inspect Rustdoc examples, resolve type aliases, or infer
the meaning of booleans inside containers; review those cases when designing an
API. Run the focused check with:

```bash
nix build .#checks.x86_64-linux.cuenv-rust-parameter-style -L --accept-flake-config
```

Replace `x86_64-linux` with the host system when needed.
When adding an application or tool outside the source trees already listed in
`nix/source-files.json`, add its Rust files to `rustStyleFiles` or its source
directories to `rustStyleInputTrees`. This keeps the check and its task inputs
in sync without including Cargo build outputs. A Rust source outside `crates`,
`apps`, and `tools` fails the check until that root is added to the checker and
the flake's scanner-root guard.

To apply Rust formatting to each workspace with the repository's pinned toolchain:

```bash
nix develop --command cargo fmt --all -- --config imports_granularity=Module,hex_literal_case=Lower,format_macro_matchers=true
nix develop --command cargo fmt --manifest-path apps/cuetty/Cargo.toml --all -- --config imports_granularity=Module,hex_literal_case=Lower,format_macro_matchers=true
nix develop --command cargo fmt --manifest-path tools/function-enum/Cargo.toml --all -- --config imports_granularity=Module,hex_literal_case=Lower,format_macro_matchers=true
```

These settings are passed explicitly because the pinned stable `rustfmt` does
not apply them from a configuration file. `cuenv fmt --fix` applies the usual
formatting but does not pass these options. The Nix checks also verify that their
formatters apply them.

````rust
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
/// # use std::path::Path;
/// let json = cuengine::evaluate_cue_package(Path::new("./config"), "cuenv")?;
/// assert!(json.contains("env"));
/// # Ok::<_, cuengine::CueEngineError>(())
/// ```
pub fn load_manifest(dir: &Path) -> cuengine::Result<Project> {
    cuengine::evaluate_cue_package_typed(dir, "cuenv")
}
````

#### Error Handling

Use structured error types with `thiserror`:

```rust
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MyError {
    #[error("Configuration error: {message}")]
    Config { message: String },

    #[error("IO error")]
    Io(#[from] std::io::Error),
}
```

Mark public error enums `#[non_exhaustive]` when downstream callers should
handle future variants. Some internal contracts deliberately require exhaustive
matching, such as the CLI's mapping of core errors to exit codes. A constructor
that returns a `Result` is named `try_new`, never `new`.

#### Public API Shape

- Use a newtype, not a `type` alias, when values are easy to mix up or the
  underlying representation may change (for example versions, version
  requirements and URLs).
- Prefer structs with named fields over public tuples or tuple aliases.

#### Locks and Panics

- Never recover a poisoned `Mutex` or `RwLock` with
  `PoisonError::into_inner`. A poisoned lock means another thread panicked
  part-way through an update, so treat it as fatal with an `expect` that states
  the invariant, or fail closed. Secret redaction must always fail closed.
- Do not rely on `std::panic::catch_unwind` for behaviour. Release builds use
  `panic = "abort"`, so a catch-and-fallback path never runs in shipped
  binaries. Avoid the panicking call instead.

#### Module Layout

A `mod.rs` that declares submodules contains only `mod` declarations,
re-exports and a small amount of glue. Implementation lives in leaf modules
named after their primary item.

#### Testing

The sandboxed Nix nextest check excludes the host-only Nix runtime tests. With
a host Nix daemon, set `CUENV_TEST_NIXPKGS` to the local store path of the
flake's pinned nixpkgs input, then run:

```bash
cuenv exec -- cargo test -p cuenv --test nix_flake_hook --test nix_flake_path -- --ignored
```

The full flake gate runs the workspace nextest suite, BDD scenarios (including
failure on undefined or skipped steps), all-feature
Rustdoc tests, Cuetty's ignored PTY tests, the Go bridge scaling test, and the
ignored infrastructure suites against local test services. To run every test,
also execute these host-only partitions in the Nix development shell:

| Partition | Command and prerequisites |
| --- | --- |
| Task-graph stress | `cargo test --locked -p cuenv --test stress_tests -- --ignored` |
| Provider registry | `cargo test --locked -p cuenv-infrastructure --test provider_end_to_end installs_provider_from_registry -- --ignored --exact`; requires network access |
| Remote cache | Start a disposable Bazel Remote server, set `CUENV_REAPI_TEST_ENDPOINT`, then run `cargo test --locked -p cuenv-cas-remote --test external_reapi -- --ignored` |
| CLI fake provider | Build `.#checks.x86_64-linux.cuenv-fake-terraform-provider`, set `CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER` to its binary, then run `cargo test --locked -p cuenv --lib commands::infrastructure::evaluation::tests::nested_native_reference_plans_and_applies_with_fake_provider -- --ignored --exact` |
| Live D1 | Set `CUENV_INFRASTRUCTURE_TEST_D1_ACCOUNT_ID`, `CUENV_INFRASTRUCTURE_TEST_D1_DATABASE_ID`, and `CLOUDFLARE_D1_API_TOKEN` for a disposable database, then run `cargo test --locked -p cuenv-infrastructure --lib state::d1::tests::lifecycle_against_d1 -- --ignored --exact` |

The host Nix partition above requires a running Nix daemon. Remove disposable
servers, caches, and the D1 database after their tests. A sandboxed nextest run
alone does not execute these host and network cases. Platform-specific tests
also require their respective operating systems.

Write tests using Rust's built-in test framework:

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
│   ├── cuenv-events/      # Event system
│   ├── cuenv-ci/          # CI support
│   └── cuenv-release/     # Release management
├── integrations/          # Editor integrations (VSCode)
├── docs/                  # Documentation source
├── examples/             # Example configurations
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

Documentation uses [Nimbus](https://nimbus-docs.com/) on [Astro](https://astro.build/):

- Source files in `docs/src/content/docs/`
- Configuration and sidebar in `docs/astro.config.ts`
- Build with `bun run build`; check links and frontmatter with `bun run check`

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
