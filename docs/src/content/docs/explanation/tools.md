---
title: Tools System
description: Understanding cuenv's hermetic tool management architecture
---

This page explains the design and internals of cuenv's tools system. For practical usage, see the [Tools How-To Guide](/how-to/tools/).

## Design Philosophy

cuenv's tools system provides **hermetic, reproducible CLI tools** without requiring global installation. The key principles are:

1. **Version pinning**: Every tool has an explicit version locked in `cuenv.lock`
2. **Platform isolation**: Tools are resolved per-platform, supporting cross-platform teams
3. **Content-addressed caching**: Tools are cached by SHA256 digest for integrity
4. **Multiple sources**: Support for Homebrew, GitHub, OCI, Nix, and Rustup

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────┐
│                      env.cue                                 │
│  runtime: #ToolsRuntime & { tools: { jq: "1.7.1" } }        │
└──────────────────────────┬──────────────────────────────────┘
                           │ cuenv sync lock
                           ▼
┌─────────────────────────────────────────────────────────────┐
│                    Tool Resolution                           │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐         │
│  │  Homebrew   │  │   GitHub    │  │     OCI     │  ...    │
│  │  Provider   │  │  Provider   │  │  Provider   │         │
│  └──────┬──────┘  └──────┬──────┘  └──────┬──────┘         │
│         └────────────────┼────────────────┘                 │
│                          ▼                                   │
│               ToolRegistry.find_for_source()                 │
└──────────────────────────┬──────────────────────────────────┘
                           │
                           ▼
┌─────────────────────────────────────────────────────────────┐
│                      cuenv.lock                              │
│  Per-platform resolved tools with SHA256 digests            │
└──────────────────────────┬──────────────────────────────────┘
                           │ cuenv tools download
                           ▼
┌─────────────────────────────────────────────────────────────┐
│                    Tool Cache                                │
│  ~/.cache/cuenv/tools/<provider>/<name>/<version>/bin/      │
└──────────────────────────┬──────────────────────────────────┘
                           │ cuenv exec / cuenv task
                           ▼
┌─────────────────────────────────────────────────────────────┐
│                    PATH Activation                           │
│  Prepends tool bin directories to PATH                      │
└─────────────────────────────────────────────────────────────┘
```

## Provider System

The tools system uses a pluggable provider architecture. Each provider implements the `ToolProvider` trait:

```rust
#[async_trait]
pub trait ToolProvider: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn can_handle(&self, source: &ToolSource) -> bool;

    async fn resolve(
        &self,
        tool_name: &str,
        version: &str,
        platform: &Platform,
        config: &serde_json::Value,
    ) -> Result<ResolvedTool>;

    async fn fetch(
        &self,
        resolved: &ResolvedTool,
        options: &ToolOptions
    ) -> Result<FetchedTool>;

    fn is_cached(&self, resolved: &ResolvedTool, options: &ToolOptions) -> bool;
    async fn check_prerequisites(&self) -> Result<()>;
}
```

### Provider Implementations

| Provider     | Source Type  | Prerequisites            | Use Case                            |
| ------------ | ------------ | ------------------------ | ----------------------------------- |
| **Homebrew** | `#Homebrew`  | None (fetches bottles)   | Default for most tools              |
| **GitHub**   | `#GitHub`    | None                     | Tools with GitHub Releases          |
| **OCI**      | `#Oci`       | None                     | Tools distributed as containers     |
| **Nix**      | `#Nix`       | `nix` CLI                | Complex toolchains                  |
| **Rustup**   | `#Rustup`    | `rustup` CLI             | Rust toolchains with components     |

### Provider Registry

Providers are registered in a `ToolRegistry` that routes tool sources to the appropriate provider:

```rust
let mut registry = ToolRegistry::new();
registry.register(HomebrewToolProvider::new());
registry.register(GitHubToolProvider::new());
registry.register(NixToolProvider::with_flakes(flakes));
registry.register(RustupToolProvider::new());

// Find provider for a source
let source = ToolSource::GitHub { repo: "jqlang/jq", ... };
let provider = registry.find_for_source(&source);
```

## Resolution Flow

### 1. Source Selection

When resolving a tool, cuenv determines the source based on:

1. **Default source**: If only a version string is provided, Homebrew is used
2. **Explicit source**: The `source` field specifies a different provider
3. **Platform overrides**: The `overrides` array can specify per-platform sources

```cue
tools: {
    // Uses Homebrew (default)
    jq: "1.7.1"

    // Explicit GitHub source
    gh: {
        version: "2.62.0"
        source: schema.#GitHub & { repo: "cli/cli", ... }
    }

    // Platform-specific overrides
    bun: {
        version: "1.3.5"
        source: schema.#Homebrew
        overrides: [
            {os: "linux", source: schema.#Oci & { image: "oven/bun:1.3.5", ... }}
        ]
    }
}
```

### 2. Platform Matching

Override matching follows specificity rules:

1. Exact match (`os` AND `arch` both match)
2. OS-only match (`os` matches, `arch` not specified)
3. Arch-only match (`arch` matches, `os` not specified)
4. Default source (no override matches)

### 3. Template Expansion

GitHub and OCI sources support template variables:

| Variable    | Description      | Example Values         |
| ----------- | ---------------- | ---------------------- |
| `{version}` | Tool version     | "1.7.1", "2.62.0"      |
| `{os}`      | Operating system | "darwin", "linux"      |
| `{arch}`    | Architecture     | "aarch64", "x86_64"    |

```cue
source: schema.#GitHub & {
    repo: "jqlang/jq"
    tag: "jq-{version}"
    asset: "jq-{os}-{arch}"  // becomes "jq-darwin-aarch64"
}
```

## Caching System

### Cache Directory Structure

```
~/.cache/cuenv/tools/
├── github/
│   └── jq/
│       └── 1.7.1/
│           ├── bin/
│           │   └── jq           # The binary
│           └── metadata.json    # Resolution metadata
├── homebrew/
│   └── ripgrep/
│       └── 14.1.1/
│           └── bin/
│               └── rg
├── nix/
│   └── profiles/
│       └── <project-hash>/      # Per-project Nix profile
└── rustup/
    └── toolchains/
        └── 1.83.0-aarch64-apple-darwin/
```

### Cache Key Components

Cache keys are derived from:

- Tool name
- Version
- Platform (os-arch)
- Provider type
- Source-specific data (repo, tag, asset for GitHub)

### Cache Invalidation

Caches are invalidated when:

1. Version changes in `env.cue`
2. Source configuration changes
3. `cuenv.lock` is regenerated with `cuenv sync lock`
4. Manual deletion of cache directory
5. `force_refetch` option is used

## Lockfile Format

The `cuenv.lock` file contains fully resolved tools for all platforms:

```yaml
version: 1
tools:
  jq:
    darwin-arm64:
      version: "1.7.1"
      provider: github
      source:
        type: github
        repo: jqlang/jq
        tag: jq-1.7.1
        asset: jq-macos-arm64
      sha256: abc123...
    linux-x86_64:
      version: "1.7.1"
      provider: github
      source:
        type: github
        repo: jqlang/jq
        tag: jq-1.7.1
        asset: jq-linux-amd64
      sha256: def456...
```

### Lockfile Benefits

1. **Reproducibility**: Same versions on every machine
2. **Offline support**: Pre-download tools with `cuenv tools download`
3. **Audit trail**: Full provenance for security scanning
4. **Fast activation**: No resolution needed at runtime

## Activation Mechanism

### Automatic Activation

When using `cuenv exec` or `cuenv task`, tools are activated automatically:

1. Read `cuenv.lock` for current platform
2. Check cache for each tool
3. Download missing tools (parallel)
4. Prepend tool directories to `PATH`
5. Set library paths (`DYLD_LIBRARY_PATH` on macOS, `LD_LIBRARY_PATH` on Linux)
6. Execute command

### Manual Activation

For interactive shells, use the `#ToolsActivate` hook:

```cue
hooks: {
    onEnter: {
        tools: schema.#ToolsActivate
    }
}
```

Or activate directly:

```bash
eval "$(cuenv tools activate)"
```

## Nix Integration

Nix tools use a different model than other providers:

1. **Profile-based**: Instead of copying binaries, creates a Nix profile
2. **Closure preservation**: Entire dependency tree is available
3. **Reproducibility**: Bit-for-bit identical builds via content-addressing

```
~/.cache/cuenv/tools/nix/profiles/<project-hash>/
├── bin/
│   ├── jq -> /nix/store/xxx-jq-1.7.1/bin/jq
│   └── python -> /nix/store/yyy-python3-3.11/bin/python
└── manifest.json
```

## Rustup Integration

The Rustup provider manages complete Rust toolchains:

1. **Toolchain management**: Installs via `rustup toolchain install`
2. **Component support**: Adds clippy, rustfmt, rust-src, etc.
3. **Target support**: Enables cross-compilation targets
4. **Profile selection**: minimal, default, or complete

```
~/.rustup/toolchains/
└── 1.83.0-aarch64-apple-darwin/
    └── bin/
        ├── cargo
        ├── rustc
        ├── clippy-driver
        └── rustfmt
```

The provider's cache key includes the profile, components, and targets to ensure proper reinstallation when configuration changes.

## Security Considerations

### Binary Verification

1. **SHA256 hashing**: All downloaded binaries are verified against lockfile digests
2. **Signature verification**: GitHub assets can leverage release signatures
3. **Nix content-addressing**: Nix store paths are content-addressed

### Supply Chain

1. **Source pinning**: Lockfile contains exact versions and digests
2. **Reproducible resolution**: Same config produces identical lockfile
3. **Audit support**: `cuenv tools list` shows full provenance

## Performance Optimizations

1. **Parallel downloads**: Multiple tools downloaded concurrently
2. **Lazy evaluation**: Tools only fetched when needed
3. **Shared cache**: Team members share cache via CUENV_CACHE_DIR
4. **Atomic extraction**: Temporary directories prevent partial extractions

## See Also

- [Tools How-To Guide](/how-to/tools/) - Practical usage
- [Configuration Schema](/reference/cue-schema/#toolsruntime) - Schema reference
- [CLI Reference](/reference/cli/#cuenv-tools) - Command documentation
- [Examples](/reference/examples/#tools-management) - Practical examples
