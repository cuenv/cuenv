---
title: Tools
description: Configure and manage hermetic, reproducible development tools
---

cuenv's tools feature provides hermetic, reproducible CLI tools managed via CUE configuration. Instead of relying on globally installed tools, cuenv downloads and manages versioned binaries from multiple sources (GitHub Releases, OCI images, Nix, Rustup).

## Quick Start

Add a `runtime` block to your `env.cue`. The easiest way is to use pre-configured contrib modules:

```cue
package cuenv

import (
    "github.com/cuenv/cuenv/schema"
    xTools "github.com/cuenv/cuenv/contrib/tools"
)

schema.#Project & {
    name: "my-project"
}

runtime: schema.#ToolsRuntime & {
    platforms: ["darwin-arm64", "darwin-x86_64", "linux-x86_64"]
    tools: {
        jq: xTools.#Jq & {version: "1.7.1"}
        yq: xTools.#Yq & {version: "4.44.6"}
    }
}

tasks: {
    process: {
        command: "jq"
        args: [".data", "input.json"]
    }
}
```

Lock the tools to create `cuenv.lock`:

```bash
cuenv sync lock
```

Run tasks - tools are activated automatically:

```bash
cuenv task process
```

## Defining Tools

### Simple Version Strings

The simplest form uses just a version string, but you must specify a source using the full `#Tool` definition or platform-specific overrides:

```cue
tools: {
    jq: {
        version: "1.7.1"
        source: schema.#GitHub & {
            repo: "jqlang/jq"
            asset: "jq-{os}-{arch}"
        }
    }
    yq: {
        version: "4.44.6"
        source: schema.#GitHub & {
            repo: "mikefarah/yq"
            tag: "v{version}"
            asset: "yq_{os}_{arch}.tar.gz"
            path: "yq_{os}_{arch}"
        }
    }
}
```

### Full Tool Specification

Use the full `#Tool` definition to specify the source and version:

```cue
tools: {
    go: {
        version: "1.24.0"
        source: schema.#Nix & {
            flake: "nixpkgs"
            package: "go_1_24"
        }
    }
}
```

The `#Tool` type supports these fields:

| Field       | Type             | Required | Description                                             |
| ----------- | ---------------- | -------- | ------------------------------------------------------- |
| `version`   | `string`         | Yes      | Tool version                                            |
| `as`        | `string`         | No       | Rename binary in PATH                                   |
| `source`    | `#Source`        | No       | Tool source (must be specified via source or overrides) |
| `overrides` | `[...#Override]` | No       | Platform-specific sources                               |

## Tool Sources

cuenv supports four tool sources:

### GitHub Releases

Downloads binaries from GitHub Releases. Supports template variables:

- `{version}` - the tool version
- `{os}` - normalized OS name (darwin, linux)
- `{arch}` - normalized architecture (arm64, x86_64)

```cue
tools: {
    gh: {
        version: "2.62.0"
        source: schema.#GitHub & {
            repo: "cli/cli"
            tag: "v{version}"
            asset: "gh_{version}_{os}_{arch}.tar.gz"
            path: "gh_{version}_{os}_{arch}/bin/gh"
        }
    }
}
```

### OCI Images

Extracts binaries from OCI container images:

```cue
tools: {
    kubectl: {
        version: "1.31.0"
        source: schema.#Oci & {
            image: "bitnami/kubectl:{version}"
            path: "/opt/bitnami/kubectl/bin/kubectl"
        }
    }
}
```

### Nix

Builds tools from Nix flakes. Requires the `nix` CLI to be installed.

```cue
runtime: schema.#ToolsRuntime & {
    platforms: ["darwin-arm64", "linux-x86_64"]
    flakes: {
        nixpkgs: "github:NixOS/nixpkgs/nixos-24.05"
    }
    tools: {
        hello: {
            version: "2.12"
            source: schema.#Nix & {
                flake: "nixpkgs"
                package: "hello"
            }
        }
    }
}
```

### Rustup

Manages complete Rust toolchains via rustup. This is the recommended source for Rust development, as it handles toolchain management, components, and cross-compilation targets.

```cue
runtime: schema.#ToolsRuntime & {
    platforms: ["darwin-arm64", "linux-x86_64"]
    tools: {
        rust: {
            version: "1.83.0"
            source: schema.#Rustup & {
                toolchain: "1.83.0"
                profile: "default"
                components: ["clippy", "rustfmt", "rust-src"]
                targets: ["x86_64-unknown-linux-gnu", "wasm32-unknown-unknown"]
            }
        }
    }
}
```

**Fields:**

| Field        | Type          | Default     | Description                                                |
| ------------ | ------------- | ----------- | ---------------------------------------------------------- |
| `toolchain`  | `string`      | required    | Toolchain identifier (e.g., "stable", "1.83.0", "nightly") |
| `profile`    | `string`      | `"default"` | Installation profile: "minimal", "default", or "complete"  |
| `components` | `[...string]` | `[]`        | Additional components (e.g., "clippy", "rustfmt")          |
| `targets`    | `[...string]` | `[]`        | Cross-compilation targets                                  |

**Profiles:**

- `minimal`: rustc, rust-std, cargo only
- `default`: minimal + rustfmt, clippy
- `complete`: All available components

:::note[Prerequisite]
Rustup must be installed on the system. Install via: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
:::

## Locking Tools

Run `cuenv sync lock` to resolve all tools for all configured platforms and write `cuenv.lock`:

```bash
cuenv sync lock
```

The lockfile contains:

- Exact versions and digests for each tool
- Platform-specific resolutions
- Provider metadata

Commit `cuenv.lock` to your repository for reproducible builds across machines.

## Automatic Activation

:::tip[No Hook Required]
When using `cuenv exec` or `cuenv task`, tools from the lockfile are activated automatically. You don't need to configure a hook for tool activation.
:::

cuenv automatically:

1. Reads `cuenv.lock` to find tools for the current platform
2. Downloads tools if not already cached
3. Prepends tool directories to `PATH`
4. Sets library paths (`DYLD_LIBRARY_PATH` on macOS, `LD_LIBRARY_PATH` on Linux)

```bash
# Tools are available immediately
cuenv exec -- jq --version
cuenv task build  # Tasks can use managed tools
```

## Manual Activation

### Shell Integration with #ToolsActivate

For interactive shell use, add the `#ToolsActivate` hook:

```cue
hooks: {
    onEnter: {
        tools: schema.#ToolsActivate
    }
}
```

This runs `cuenv tools activate` when entering the directory, adding tools to your shell's PATH.

### Scripting

For scripts, use `cuenv tools activate` directly:

```bash
eval "$(cuenv tools activate)"
```

## Managing Tools

### Pre-download Tools

Download all tools for the current platform:

```bash
cuenv tools download
```

This is useful for CI caching or offline use.

### List Configured Tools

View all tools and their per-platform resolutions:

```bash
cuenv tools list
```

Output shows tool names, versions, providers, and digests for each platform.

## Platform-Specific Overrides

Use `overrides` to specify different sources per platform:

```cue
tools: {
    bun: {
        version: "1.3.5"
        overrides: [
            // Use GitHub on macOS
            {
                os: "darwin"
                source: schema.#GitHub & {
                    repo: "oven-sh/bun"
                    tag: "bun-v{version}"
                    asset: "bun-darwin-{arch}.zip"
                    path: "bun-darwin-{arch}/bun"
                }
            }
            // Use OCI image on Linux
            {
                os: "linux"
                source: schema.#Oci & {
                    image: "oven/bun:{version}"
                    path: "/usr/local/bin/bun"
                }
            }
        ]
    }
}
```

Override matching:

- `os` matches the operating system (darwin, linux)
- `arch` matches the architecture (arm64, x86_64)
- More specific overrides take precedence

## Contrib Modules

cuenv provides pre-configured tool definitions in `contrib/` for tools with complex platform-specific requirements. These save you from writing boilerplate overrides.

### Using Contrib Modules

Import and use contrib modules in your `env.cue`:

```cue
package cuenv

import (
    "github.com/cuenv/cuenv/schema"
    xTools "github.com/cuenv/cuenv/contrib/tools"
    xRust "github.com/cuenv/cuenv/contrib/rust"
    xBun "github.com/cuenv/cuenv/contrib/bun"
)

runtime: schema.#ToolsRuntime & {
    platforms: ["darwin-arm64", "darwin-x86_64", "linux-x86_64"]
    flakes: nixpkgs: "github:NixOS/nixpkgs/nixos-24.11"
    tools: {
        // Generic tools from contrib/tools
        jq: xTools.#Jq & {version: "1.7.1"}
        yq: xTools.#Yq & {version: "4.44.6"}
        cue: xTools.#Cue & {version: "0.15.3"}
        treefmt: xTools.#Treefmt & {version: "2.4.0"}

        // Rust toolchain via rustup
        rust: xRust.#Rust & {version: "1.83.0"}
        "rust-analyzer": xRust.#RustAnalyzer & {version: "2025-12-29"}
        "cargo-nextest": xRust.#CargoNextest & {version: "0.9.116"}

        // Bun runtime
        bun: xBun.#Bun & {version: "1.3.5"}
    }
}
```

### contrib/tools

Generic development tools fetched from GitHub Releases with pre-configured platform overrides.

| Definition | Tool    | Description                                        |
| ---------- | ------- | -------------------------------------------------- |
| `#Jq`      | jq      | JSON processor from jqlang/jq                      |
| `#Yq`      | yq      | YAML processor from mikefarah/yq (uses `v` prefix) |
| `#Cue`     | cue     | CUE language CLI from cue-lang/cue                 |
| `#Treefmt` | treefmt | Multi-language formatter from numtide/treefmt      |

**Example:**

```cue
import xTools "github.com/cuenv/cuenv/contrib/tools"

runtime: schema.#ToolsRuntime & {
    platforms: ["darwin-arm64", "linux-x86_64"]
    tools: {
        jq: xTools.#Jq & {version: "1.7.1"}
        yq: xTools.#Yq & {version: "4.44.6"}
    }
}
```

### contrib/rust

Rust ecosystem tools including the complete toolchain via rustup and popular cargo extensions.

| Definition        | Tool            | Source | Description                                  |
| ----------------- | --------------- | ------ | -------------------------------------------- |
| `#Rust`           | rust            | Rustup | Full Rust toolchain (cargo, rustc, etc.)     |
| `#RustAnalyzer`   | rust-analyzer   | GitHub | LSP server (uses date-based versions)        |
| `#CargoNextest`   | cargo-nextest   | GitHub | Fast test runner                             |
| `#CargoDeny`      | cargo-deny      | GitHub | License/security checker (no `v` tag prefix) |
| `#CargoLlvmCov`   | cargo-llvm-cov  | GitHub | Code coverage via LLVM                       |
| `#CargoCyclonedx` | cargo-cyclonedx | Nix    | SBOM generation (requires `runtime.flakes`)  |
| `#CargoZigbuild`  | cargo-zigbuild  | GitHub | Cross-compilation with Zig                   |
| `#SccacheTool`    | sccache         | GitHub | Compilation caching                          |
| `#Zig`            | zig             | Nix    | Zig toolchain (requires `runtime.flakes`)    |

**Example - Full Rust development setup:**

```cue
import xRust "github.com/cuenv/cuenv/contrib/rust"

runtime: schema.#ToolsRuntime & {
    platforms: ["darwin-arm64", "linux-x86_64"]
    flakes: nixpkgs: "github:NixOS/nixpkgs/nixos-24.11"
    tools: {
        // Rust toolchain with components
        rust: xRust.#Rust & {
            version: "1.83.0"
            source: {
                profile: "default"
                components: ["clippy", "rustfmt", "rust-src"]
            }
        }

        // LSP support (date-based version)
        "rust-analyzer": xRust.#RustAnalyzer & {version: "2025-12-29"}

        // Testing and coverage
        "cargo-nextest": xRust.#CargoNextest & {version: "0.9.116"}
        "cargo-llvm-cov": xRust.#CargoLlvmCov & {version: "0.7.0"}

        // Security and licensing
        "cargo-deny": xRust.#CargoDeny & {version: "0.18.9"}

        // Cross-compilation (requires Zig)
        "cargo-zigbuild": xRust.#CargoZigbuild & {version: "0.20.5"}
        zig: xRust.#Zig & {version: "0.14.0"}

        // Build caching
        sccache: xRust.#SccacheTool & {version: "0.10.0"}
    }
}
```

### contrib/bun

Bun JavaScript runtime with platform-specific asset handling (Bun uses non-standard arch naming).

| Definition | Tool | Description                                            |
| ---------- | ---- | ------------------------------------------------------ |
| `#Bun`     | bun  | Bun runtime - handles aarch64/x64 naming automatically |

**Example:**

```cue
import xBun "github.com/cuenv/cuenv/contrib/bun"

runtime: schema.#ToolsRuntime & {
    platforms: ["darwin-arm64", "linux-x86_64"]
    tools: {
        bun: xBun.#Bun & {version: "1.3.5"}
    }
}
```

### Creating Custom Contrib-Style Definitions

You can create your own reusable tool definitions following the same pattern:

```cue
package mytools

import "github.com/cuenv/cuenv/schema"

#MyTool: schema.#Tool & {
    version!: string
    overrides: [
        {os: "darwin", arch: "arm64", source: schema.#GitHub & {
            repo: "org/repo"
            asset: "mytool-darwin-arm64.tar.gz"
            path: "mytool"
        }},
        {os: "linux", arch: "x86_64", source: schema.#GitHub & {
            repo: "org/repo"
            asset: "mytool-linux-amd64.tar.gz"
            path: "mytool"
        }},
    ]
}
```

## Troubleshooting

### Tool Not Found After Locking

```
error: tool 'mytool' not found in lockfile for platform darwin-arm64
```

**Fix:** Ensure the platform is listed in `runtime.platforms`, then run `cuenv sync lock`.

### Binary Not Available for Platform

```
error: tool 'mytool' could not be resolved for platform darwin-arm64
```

**Fix:** The tool may not have a binary available for your platform. Check if the source (GitHub, OCI, Nix) provides builds for your platform, or use platform-specific overrides to specify different sources.

### Nix Prerequisites Missing

```
error: prerequisite not met: nix CLI not installed
```

**Fix:** Install Nix and ensure it's in your PATH:

```bash
curl --proto '=https' --tlsv1.2 -sSf -L https://install.determinate.systems/nix | sh
```

### Tool Download Fails

```
error: failed to fetch tool: network error
```

**Fix:** Check network connectivity. For CI, consider using `cuenv tools download` as a cached step before running tasks.

## See Also

- [Tools Architecture](/explanation/tools/) - How the tools system works internally
- [Tools Examples](/reference/examples/#tools-management) - Practical configuration examples
- [CLI Reference: cuenv tools](/reference/cli/#cuenv-tools) - Command documentation
- [CUE Schema: #ToolsRuntime](/reference/cue-schema/#toolsruntime) - Schema reference
- [Nix Integration](/how-to/nix/) - Using Nix with cuenv
