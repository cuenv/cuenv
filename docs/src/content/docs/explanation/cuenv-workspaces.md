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
