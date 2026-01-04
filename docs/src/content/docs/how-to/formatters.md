---
title: Formatters
description: Format code consistently across your project
---

cuenv provides built-in code formatting with `cuenv fmt`. Configure formatters once in your `env.cue` and format your entire codebase with a single command.

## Quick Start

**1. Add formatters to your `env.cue`:**

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
    name: "my-project"

    formatters: {
        rust: {}
        nix: { tool: "nixfmt" }
        go: {}
        cue: {}
    }
}
```

**2. Check formatting:**

```bash
cuenv fmt
```

**3. Fix formatting issues:**

```bash
cuenv fmt --fix
```

That's it! cuenv discovers all matching files (respecting `.gitignore`) and formats them.

## Commands

### Check Mode (Default)

```bash
cuenv fmt
```

Validates that all files are properly formatted without making changes. Exits with code `3` if any files need formatting—perfect for CI pipelines.

### Fix Mode

```bash
cuenv fmt --fix
```

Formats all files in-place. This is what you'll use during development.

### Filter by Formatter

```bash
# Only check Rust files
cuenv fmt --only rust

# Fix only Go and CUE files
cuenv fmt --fix --only go,cue

# Check Nix files only
cuenv fmt --only nix
```

The `--only` flag accepts a comma-separated list: `rust`, `nix`, `go`, `cue`.

### Format a Specific Path

```bash
# Format files in a subdirectory
cuenv fmt --fix -p ./packages/my-app
```

## Supported Formatters

### Rust (`rustfmt`)

```cue
formatters: {
    rust: {
        enabled:  true           // default: true
        includes: ["**/*.rs"]    // default: ["*.rs"]
        edition:  "2024"         // optional: "2018" | "2021" | "2024"
    }
}
```

| Field      | Type          | Default    | Description                       |
| ---------- | ------------- | ---------- | --------------------------------- |
| `enabled`  | `bool`        | `true`     | Enable/disable this formatter     |
| `includes` | `[...string]` | `["*.rs"]` | Glob patterns for files to format |
| `edition`  | `string`      | —          | Rust edition for formatting rules |

### Nix (`nixfmt` or `alejandra`)

```cue
formatters: {
    nix: {
        enabled:  true           // default: true
        includes: ["**/*.nix"]   // default: ["*.nix"]
        tool:     "alejandra"    // default: "nixfmt"
    }
}
```

| Field      | Type          | Default     | Description                              |
| ---------- | ------------- | ----------- | ---------------------------------------- |
| `enabled`  | `bool`        | `true`      | Enable/disable this formatter            |
| `includes` | `[...string]` | `["*.nix"]` | Glob patterns for files to format        |
| `tool`     | `string`      | `"nixfmt"`  | Tool to use: `"nixfmt"` or `"alejandra"` |

### Go (`gofmt`)

```cue
formatters: {
    go: {
        enabled:  true           // default: true
        includes: ["**/*.go"]    // default: ["*.go"]
    }
}
```

| Field      | Type          | Default    | Description                       |
| ---------- | ------------- | ---------- | --------------------------------- |
| `enabled`  | `bool`        | `true`     | Enable/disable this formatter     |
| `includes` | `[...string]` | `["*.go"]` | Glob patterns for files to format |

### CUE (`cue fmt`)

```cue
formatters: {
    cue: {
        enabled:  true           // default: true
        includes: ["**/*.cue"]   // default: ["*.cue"]
    }
}
```

| Field      | Type          | Default     | Description                       |
| ---------- | ------------- | ----------- | --------------------------------- |
| `enabled`  | `bool`        | `true`      | Enable/disable this formatter     |
| `includes` | `[...string]` | `["*.cue"]` | Glob patterns for files to format |

## Pattern Matching

Patterns use glob syntax and match against paths relative to your project root:

| Pattern                | Matches                                 |
| ---------------------- | --------------------------------------- |
| `*.rs`                 | Rust files in project root only         |
| `**/*.rs`              | All Rust files recursively              |
| `src/**/*.rs`          | Rust files under `src/`                 |
| `crates/*/src/**/*.rs` | Rust files in any crate's src directory |
| `!vendor/**`           | Exclude vendor directory                |

:::tip
Use `**/*.ext` patterns to recursively match files. The default patterns (`*.rs`, `*.nix`, etc.) only match files in the project root.
:::

## CI Integration

Add formatting checks to your CI pipeline:

```yaml
# GitHub Actions
- name: Check formatting
  run: cuenv fmt

# GitLab CI
format:
  script:
    - cuenv fmt
```

The command exits with code `3` if any files need formatting, failing the CI job.

### Pre-commit Hook

Add to `.pre-commit-config.yaml`:

```yaml
repos:
  - repo: local
    hooks:
      - id: cuenv-fmt
        name: cuenv fmt
        entry: cuenv fmt
        language: system
        pass_filenames: false
```

Or use a simple Git hook in `.git/hooks/pre-commit`:

```bash
#!/bin/sh
cuenv fmt || {
    echo "Formatting check failed. Run 'cuenv fmt --fix' to fix."
    exit 1
}
```

## Examples

### Rust Workspace

```cue
formatters: {
    rust: {
        includes: [
            "crates/**/*.rs",
            "tests/**/*.rs",
            "benches/**/*.rs",
        ]
        edition: "2024"
    }
}
```

### Nix Flake Project

```cue
formatters: {
    nix: {
        tool: "alejandra"
        includes: [
            "*.nix",
            "nix/**/*.nix",
            "modules/**/*.nix",
        ]
    }
}
```

### Multi-Language Monorepo

```cue
formatters: {
    rust: {
        includes: ["services/**/*.rs", "libs/**/*.rs"]
        edition: "2024"
    }
    go: {
        includes: ["cmd/**/*.go", "pkg/**/*.go", "internal/**/*.go"]
    }
    cue: {
        includes: ["**/*.cue"]
    }
    nix: {
        tool: "nixfmt"
        includes: ["*.nix", "nix/**/*.nix"]
    }
}
```

### Selective Formatting

```cue
formatters: {
    // Format Rust with specific edition
    rust: {
        includes: ["src/**/*.rs"]
        edition: "2024"
    }
    // Skip Go formatting entirely
    go: {
        enabled: false
    }
    // Use alejandra for Nix
    nix: {
        tool: "alejandra"
    }
}
```

## Post-Sync Formatting

When using [cuenv codegen](/how-to/codegen/) for code generation, formatters also run automatically after `cuenv sync codegen`:

```bash
# Generates files AND formats them
cuenv sync codegen
```

This ensures generated code matches your project's style without a separate step. The same formatter configuration applies to both `cuenv fmt` and post-sync formatting.

## Troubleshooting

### "No formatters configured"

You need a `formatters` block in your `env.cue`:

```cue
formatters: {
    rust: {}  // Minimal config with defaults
}
```

### Formatter not found

Ensure the formatter binary is in your PATH:

```bash
# Check if rustfmt is available
which rustfmt

# Check if nixfmt is available
which nixfmt
```

### Files not being formatted

1. Check your `includes` patterns match the files:

   ```bash
   # List files that would be formatted
   cuenv fmt --only rust 2>&1 | head -20
   ```

2. Ensure files aren't in `.gitignore` (cuenv respects gitignore)

3. Verify the formatter is enabled:
   ```cue
   formatters: {
       rust: {
           enabled: true  // Must be true (default)
       }
   }
   ```

### Pattern not matching

Remember patterns are relative to project root:

```cue
// Wrong: absolute-style pattern
includes: ["/src/**/*.rs"]

// Correct: relative pattern
includes: ["src/**/*.rs"]
```
