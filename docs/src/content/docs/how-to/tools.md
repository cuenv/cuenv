---
title: Tools
description: Configure and manage hermetic, reproducible development tools
---

cuenv's tools feature provides hermetic, reproducible CLI tools managed via CUE configuration. Instead of relying on globally installed tools, cuenv downloads and manages versioned binaries from multiple sources (Homebrew, GitHub Releases, OCI images, Nix).

## Quick Start

Add a `runtime` block to your `env.cue`:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
    name: "my-project"
}

runtime: schema.#ToolsRuntime & {
    platforms: ["darwin-arm64", "darwin-x86_64", "linux-x86_64"]
    tools: {
        jq: "1.7.1"
        yq: "4.44.6"
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

The simplest form uses just a version string. This defaults to Homebrew as the source:

```cue
tools: {
    jq: "1.7.1"
    yq: "4.44.6"
    ripgrep: "14.1.1"
}
```

### Full Tool Specification

For more control, use the full `#Tool` definition:

```cue
tools: {
    go: {
        version: "1.24.0"
        source: schema.#Homebrew & {formula: "go@1.24"}
    }
}
```

The `#Tool` type supports these fields:

| Field       | Type             | Required | Description                          |
| ----------- | ---------------- | -------- | ------------------------------------ |
| `version`   | `string`         | Yes      | Tool version                         |
| `as`        | `string`         | No       | Rename binary in PATH                |
| `source`    | `#Source`        | No       | Default source (Homebrew if omitted) |
| `overrides` | `[...#Override]` | No       | Platform-specific sources            |

## Tool Sources

cuenv supports four tool sources:

### Homebrew (Default)

Fetches from Homebrew bottles hosted on `ghcr.io/homebrew`. This is the default and covers most common tools.

```cue
tools: {
    // Implicit Homebrew
    jq: "1.7.1"

    // Explicit Homebrew with formula override
    go: {
        version: "1.24.0"
        source: schema.#Homebrew & {formula: "go@1.24"}
    }
}
```

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
        source: schema.#Homebrew
        overrides: [
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

cuenv provides pre-configured tool definitions for complex cases. Import and use them:

```cue
package cuenv

import (
    "github.com/cuenv/cuenv/schema"
    xRust "github.com/cuenv/cuenv/contrib/rust"
    xBun "github.com/cuenv/cuenv/contrib/bun"
)

runtime: schema.#ToolsRuntime & {
    platforms: ["darwin-arm64", "darwin-x86_64", "linux-x86_64"]
    tools: {
        // Rust toolchain from Homebrew
        rust: xRust.#Rust & {version: "1.92.0"}
        "rust-analyzer": xRust.#RustAnalyzer & {version: "2025-12-22"}
        "cargo-nextest": xRust.#CargoNextest & {version: "0.9.116"}

        // Bun with platform-specific GitHub assets
        bun: xBun.#Bun & {version: "1.3.5"}
    }
}
```

Available contrib modules:

| Module         | Tools                                                                                                                                 |
| -------------- | ------------------------------------------------------------------------------------------------------------------------------------- |
| `contrib/rust` | `#Rust`, `#RustAnalyzer`, `#CargoNextest`, `#CargoDeny`, `#CargoLlvmCov`, `#CargoCyclonedx`, `#CargoZigbuild`, `#SccacheTool`, `#Zig` |
| `contrib/bun`  | `#Bun`                                                                                                                                |

## Troubleshooting

### Tool Not Found After Locking

```
error: tool 'mytool' not found in lockfile for platform darwin-arm64
```

**Fix:** Ensure the platform is listed in `runtime.platforms`, then run `cuenv sync lock`.

### Homebrew Bottle Not Available

```
error: no bottle available for 'formula' on darwin-arm64
```

**Fix:** The tool may not have a Homebrew bottle for your platform. Use a different source (GitHub, OCI) as an override.

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

- [CLI Reference: cuenv tools](/reference/cli/#cuenv-tools) - Command documentation
- [CUE Schema: #ToolsRuntime](/reference/cue-schema/#toolsruntime) - Schema reference
- [Nix Integration](/how-to/nix/) - Using Nix with cuenv
