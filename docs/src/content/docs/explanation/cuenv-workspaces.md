---
title: cuenv-workspaces
description: Workspace and dependency resolution across multiple package managers
---

The `cuenv-workspaces` crate provides trait-based abstractions for discovering, parsing, and resolving workspace configurations and dependencies across different package managers including npm, Bun, pnpm, Yarn (Classic and Modern), and Cargo.

## Overview

Modern projects often use multiple package managers or need to understand workspace structures. This crate provides:

- Workspace discovery (finding packages in monorepos)
- Lockfile parsing (extracting resolved dependencies)
- Dependency resolution (building dependency graphs)
- Package manager detection (from lockfiles or commands)

## Architecture

```text
┌─────────────────────────────────────────────────────────────────────────┐
│                         cuenv-workspaces                                │
│  ┌──────────────────┐  ┌──────────────────┐  ┌──────────────────────┐  │
│  │ WorkspaceDiscovery│  │ LockfileParser   │  │ DependencyResolver   │  │
│  │ (trait)          │  │ (trait)          │  │ (trait)              │  │
│  └────────┬─────────┘  └────────┬─────────┘  └──────────┬───────────┘  │
│           │                     │                       │              │
│  ┌────────┴─────────┐  ┌────────┴─────────┐  ┌──────────┴───────────┐  │
│  │ PackageJson      │  │ NpmLockfile      │  │ GenericDependency    │  │
│  │ PnpmWorkspace    │  │ BunLockfile      │  │ Resolver             │  │
│  │ CargoToml        │  │ PnpmLockfile     │  └──────────────────────┘  │
│  └──────────────────┘  │ YarnLockfile     │                            │
│                        │ CargoLockfile    │                            │
│                        └──────────────────┘                            │
└─────────────────────────────────────────────────────────────────────────┘
```

### Core Traits

**WorkspaceDiscovery**
Discovers workspace configuration from a root directory.

**LockfileParser**
Parses lockfiles into structured entries.

**DependencyResolver**
Builds dependency graphs from workspace and lockfile data.

## Feature Flags

The crate uses feature flags for minimal dependency footprint:

### Discovery Features

| Feature                  | Description                              |
| ------------------------ | ---------------------------------------- |
| `discovery-javascript`   | npm, Bun, Yarn, pnpm discovery (default) |
| `discovery-rust`         | Cargo workspace discovery                |
| `discovery-package-json` | npm/Bun/Yarn via package.json            |
| `discovery-pnpm`         | pnpm via pnpm-workspace.yaml             |
| `discovery-cargo`        | Cargo via Cargo.toml                     |

### Parser Features

| Feature               | Description                      |
| --------------------- | -------------------------------- |
| `parsers-javascript`  | All JavaScript parsers (default) |
| `parsers-rust`        | All Rust parsers                 |
| `parser-npm`          | npm's package-lock.json (v3)     |
| `parser-bun`          | Bun's bun.lock (JSONC)           |
| `parser-pnpm`         | pnpm's pnpm-lock.yaml            |
| `parser-yarn-classic` | Yarn Classic (v1.x)              |
| `parser-yarn-modern`  | Yarn Modern (v2+)                |
| `parser-cargo`        | Cargo's Cargo.lock               |

### Other Features

| Feature     | Description                         |
| ----------- | ----------------------------------- |
| `detection` | Package manager detection (default) |

### Default Features

```toml
# Full JavaScript support out-of-the-box
[dependencies]
cuenv-workspaces = "..."

# Minimal Rust-only
[dependencies]
cuenv-workspaces = { version = "...", default-features = false, features = ["discovery-rust"] }
```

## API Reference

### Core Types

```rust
use cuenv_workspaces::{
    Workspace,        // Discovered workspace with members
    WorkspaceMember,  // Single package/crate in workspace
    PackageManager,   // Enum of supported package managers
    LockfileEntry,    // Resolved dependency from lockfile
    DependencySpec,   // How a dependency is specified
};
```

### Workspace

Represents a discovered workspace:

```rust
use cuenv_workspaces::Workspace;

let workspace: Workspace = /* discovery */;

println!("Found {} members", workspace.member_count());
for member in &workspace.members {
    println!("  {} at {}", member.name, member.path.display());
}
```

### PackageManager

Supported package managers:

```rust
use cuenv_workspaces::PackageManager;

match manager {
    PackageManager::Npm => { /* npm */ }
    PackageManager::Bun => { /* bun */ }
    PackageManager::Pnpm => { /* pnpm */ }
    PackageManager::YarnClassic => { /* yarn v1 */ }
    PackageManager::YarnModern => { /* yarn v2+ */ }
    PackageManager::Cargo => { /* cargo */ }
}
```

### WorkspaceDiscovery Trait

```rust
use cuenv_workspaces::{WorkspaceDiscovery, PackageJsonDiscovery};
use std::path::Path;

let discovery = PackageJsonDiscovery;
let workspace = discovery.discover(Path::new("."))?;
```

Available implementations:

- `PackageJsonDiscovery` - npm/Bun/Yarn workspaces
- `PnpmWorkspaceDiscovery` - pnpm workspaces
- `CargoTomlDiscovery` - Cargo workspaces

### LockfileParser Trait

```rust
use cuenv_workspaces::{LockfileParser, NpmLockfileParser};
use std::path::Path;

let parser = NpmLockfileParser;
let entries = parser.parse(Path::new("package-lock.json"))?;

for entry in entries {
    println!("{} @ {}", entry.name, entry.version);
}
```

Available implementations:

- `NpmLockfileParser` - package-lock.json
- `BunLockfileParser` - bun.lock
- `PnpmLockfileParser` - pnpm-lock.yaml
- `YarnClassicLockfileParser` - yarn.lock (v1)
- `YarnModernLockfileParser` - yarn.lock (v2+)
- `CargoLockfileParser` - Cargo.lock

npm parser coverage keeps nested `node_modules` fixture setup separate from
workspace and registry assertions so npm workspace membership rules stay easy
to audit.

Node module materializer cache-directory coverage scopes `HOME` through the
test environment helper, so package-manager cache path assertions do not need
process-wide unsafe environment mutation.

Bun lockfile parsing keeps the public parser entrypoint small by separating
binary-lockfile rejection, JSONC loading, lockfile-version validation, and
entry materialization before package-specific locator parsing.

pnpm parsing keeps package-key parsing separate from source selection, so
scoped-name handling, peer suffix stripping, and tarball/git/path resolution
stay in focused helpers instead of one nested parser branch.
pnpm and Yarn Modern metadata fields that must be accepted for lockfile shape
compatibility but are not used for dependency entries are stored as
underscore-prefixed serde fields, making the ignored-data boundary explicit
without local dead-code suppressions.
The crate root keeps only the dependency-version lint allowance required by the
workspace dependency graph; parser and derive warnings are handled at their
actual source rather than by a crate-wide `unused_assignments` suppression.

Yarn Classic parsing uses `yarn_lock_parser` when possible and falls back to a
small parser state that handles headers, version/resolved/integrity fields, and
dependency lines without keeping the fallback path as one monolithic parser
function.

Yarn Modern parsing keeps descriptor splitting, protocol detection, and
git-resolution parsing in separate helpers so scoped package handling stays out
of the lockfile entry assembly path.

Cargo lockfile parsing separates workspace-member discovery into
`crates/workspaces/src/parsers/rust/cargo/workspace.rs`, while Cargo
`SourceId` conversion stays with lockfile entry assembly. That keeps
Cargo.toml glob/default/exclude handling separate from registry, git, path, and
unknown source fallback handling.

### Detection Functions

```rust
use cuenv_workspaces::{detect_package_managers, detect_from_command};
use std::path::Path;

// Detect from lockfiles in directory
let managers = detect_package_managers(Path::new("."))?;
for manager in managers {
    println!("Detected: {}", manager);
}

// Detect from command string
if let Some(manager) = detect_from_command("cargo build") {
    println!("Command uses: {}", manager);
}
```

Detection orchestration stays in `crates/workspaces/src/detection.rs`, while
shell command parsing lives in `detection/command.rs` and lockfile/workspace
config scanning lives in `detection/filesystem.rs`. Package.json manager hints
and fallback npm detection live in `detection/package_json.rs`; priority
ordering remains with the public entrypoints so command hints, Yarn version
handling, config validation, and confidence scoring stay in focused boundaries.

## Integration Patterns

### Discover Workspace Members

```rust
use cuenv_workspaces::{WorkspaceDiscovery, PackageJsonDiscovery};
use std::path::Path;

fn list_workspace_packages(root: &Path) -> cuenv_workspaces::Result<()> {
    let discovery = PackageJsonDiscovery;

    if let Ok(workspace) = discovery.discover(root) {
        println!("Found {} packages:", workspace.member_count());
        for member in &workspace.members {
            println!("  - {} ({})", member.name, member.path.display());
        }
    }

    Ok(())
}
```

### Parse Multiple Lockfile Types

```rust
use cuenv_workspaces::{
    LockfileParser,
    NpmLockfileParser,
    CargoLockfileParser,
    detect_package_managers,
    PackageManager,
};
use std::path::Path;

fn analyze_dependencies(root: &Path) -> cuenv_workspaces::Result<()> {
    let managers = detect_package_managers(root)?;

    for manager in managers {
        match manager {
            PackageManager::Npm => {
                let parser = NpmLockfileParser;
                let entries = parser.parse(&root.join("package-lock.json"))?;
                println!("npm: {} dependencies", entries.len());
            }
            PackageManager::Cargo => {
                let parser = CargoLockfileParser;
                let entries = parser.parse(&root.join("Cargo.lock"))?;
                println!("cargo: {} dependencies", entries.len());
            }
            _ => {}
        }
    }

    Ok(())
}
```

### Detect Package Manager from Command

```rust
use cuenv_workspaces::detect_from_command;

fn get_package_manager_for_task(command: &str) -> Option<String> {
    detect_from_command(command).map(|pm| pm.to_string())
}

// Examples:
// "npm run build" -> Some("npm")
// "cargo test" -> Some("cargo")
// "bun install" -> Some("bun")
// "python script.py" -> None
```

### Cargo Workspace Discovery

```rust
use cuenv_workspaces::{WorkspaceDiscovery, CargoTomlDiscovery};
use std::path::Path;

let discovery = CargoTomlDiscovery;
let workspace = discovery.discover(Path::new("."))?;

for member in &workspace.members {
    println!("Crate: {} at {}", member.name, member.path.display());
}
```

## Edge Case Handling

The crate handles edge cases gracefully:

### Cargo Workspaces

- **Missing `[workspace]` section**: Treated as valid empty workspace (single-package repo)
- **Missing/malformed member manifests**: Silently skipped; only valid members included

### JavaScript Workspaces

- **Missing/malformed member manifests**: Silently skipped
- **Members without `name` field**: Skipped during enumeration

This tolerant behavior ensures discovery doesn't fail due to individual member issues.

## Testing

```bash
# Run all workspace tests
cargo test -p cuenv-workspaces

# Test specific features
cargo test -p cuenv-workspaces --features parser-npm
cargo test -p cuenv-workspaces --features discovery-rust

# Test with all features
cargo test -p cuenv-workspaces --all-features
```

## See Also

- [cuengine](/explanation/cuengine/) - CUE evaluation engine
- [API Reference](/reference/rust-api/) - Complete API documentation
