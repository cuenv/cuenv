---
title: Formatters
description: Automatically format generated files after sync
---

cuenv can automatically format generated files after running `cuenv sync cubes`. This keeps your generated code consistent with your project's style guidelines without manual intervention.

## Quick Start

Add a `formatters` field to your `env.cue`:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
    name: "my-project"

    formatters: {
        rust: {
            includes: ["**/*.rs"]
        }
        nix: {
            tool: "nixfmt"
        }
    }
}
```

Then run:

```bash
cuenv sync cubes
```

Generated files matching your patterns will be automatically formatted.

## Commands

### Format generated files

```bash
cuenv sync cubes
```

Runs cube generation and formats all generated files that match your formatter patterns.

### Check formatting (CI mode)

```bash
cuenv sync cubes --check
```

Verifies generated files are properly formatted without making changes. Exits with non-zero status if formatting is needed. Useful in CI pipelines.

### Preview changes (dry-run)

```bash
cuenv sync cubes --dry-run
```

Shows what files would be generated and formatted without actually writing them.

## Supported Formatters

### Rust (rustfmt)

Formats Rust files using `rustfmt`.

```cue
formatters: {
    rust: {
        enabled:  true           // default: true
        includes: ["**/*.rs"]    // default: ["*.rs"]
        edition:  "2024"         // optional: "2018" | "2021" | "2024"
    }
}
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | `bool` | `true` | Enable/disable this formatter |
| `includes` | `[...string]` | `["*.rs"]` | Glob patterns for files to format |
| `edition` | `string` | (none) | Rust edition for formatting rules |

### Nix (nixfmt or alejandra)

Formats Nix files using either `nixfmt` or `alejandra`.

```cue
formatters: {
    nix: {
        enabled:  true           // default: true
        includes: ["**/*.nix"]   // default: ["*.nix"]
        tool:     "alejandra"    // default: "nixfmt"
    }
}
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | `bool` | `true` | Enable/disable this formatter |
| `includes` | `[...string]` | `["*.nix"]` | Glob patterns for files to format |
| `tool` | `string` | `"nixfmt"` | Formatter tool: `"nixfmt"` or `"alejandra"` |

### Go (gofmt)

Formats Go files using `gofmt`.

```cue
formatters: {
    go: {
        enabled:  true           // default: true
        includes: ["**/*.go"]    // default: ["*.go"]
    }
}
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | `bool` | `true` | Enable/disable this formatter |
| `includes` | `[...string]` | `["*.go"]` | Glob patterns for files to format |

### CUE (cue fmt)

Formats CUE files using `cue fmt`.

```cue
formatters: {
    cue: {
        enabled:  true           // default: true
        includes: ["**/*.cue"]   // default: ["*.cue"]
    }
}
```

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | `bool` | `true` | Enable/disable this formatter |
| `includes` | `[...string]` | `["*.cue"]` | Glob patterns for files to format |

## Pattern Matching

File patterns use glob syntax and are matched relative to the project root:

- `*.rs` - Rust files in the project root
- `**/*.rs` - All Rust files recursively
- `src/**/*.rs` - Rust files under src/
- `crates/*/src/**/*.rs` - Rust files in any crate's src directory

Patterns are matched against the relative path from your project root. For example, if a generated file is at `/path/to/project/src/lib.rs`, it's matched against `src/lib.rs`.

Invalid glob patterns are logged as warnings and skipped (not silent failures).

## Output Status

When formatting runs, you'll see status messages:

```
Synchronized files
Formatted 42 Rust file(s)
Formatted 12 Nix file(s)
```

In check mode, failures produce errors:

```
Error: 3 Rust file(s) need formatting
```

## Examples

### Rust Project with Edition 2024

```cue
formatters: {
    rust: {
        includes: ["src/**/*.rs", "tests/**/*.rs", "benches/**/*.rs"]
        edition:  "2024"
    }
}
```

### Nix Flake with Alejandra

```cue
formatters: {
    nix: {
        tool:     "alejandra"
        includes: ["*.nix", "nix/**/*.nix"]
    }
}
```

### Multi-language Project

```cue
formatters: {
    rust: {
        includes: ["crates/**/*.rs"]
        edition:  "2024"
    }
    go: {
        includes: ["cmd/**/*.go", "pkg/**/*.go"]
    }
    cue: {
        includes: ["schema/**/*.cue", "*.cue"]
    }
    nix: {
        tool: "nixfmt"
    }
}
```

### Disable a Formatter

```cue
formatters: {
    rust: {
        enabled: false  // Skip Rust formatting
    }
    nix: {}  // Keep Nix formatting with defaults
}
```

## Integration with CI

Use `--check` mode in your CI pipeline to verify generated files are properly formatted:

```yaml
- name: Check sync formatting
  run: cuenv sync cubes --check
```

This fails the build if any generated files need formatting, ensuring your repository stays consistent.
