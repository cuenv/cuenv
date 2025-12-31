---
title: cuenv-formatters
description: Code formatting system for cuenv projects
---

The formatters system provides code formatting for cuenv projects through two entry points:

1. **`cuenv fmt`** - Format all files in your project
2. **`cuenv sync cubes`** - Format generated files after code generation

Both use the same configuration and formatter infrastructure.

## Overview

cuenv's formatter system:

- Discovers files by walking the directory tree (respecting `.gitignore`)
- Matches files against glob patterns defined in your configuration
- Invokes external formatters (`rustfmt`, `nixfmt`, `gofmt`, `cue fmt`)
- Supports check mode for CI validation

## Architecture

```text
┌──────────────────────────────────────────────────────────────────────┐
│                           Entry Points                                │
├─────────────────────────────────┬────────────────────────────────────┤
│         cuenv fmt               │        cuenv sync cubes            │
│   (format all matching files)   │   (format generated files only)    │
└─────────────────┬───────────────┴────────────────┬───────────────────┘
                  │                                │
                  ▼                                ▼
         ┌────────────────┐               ┌────────────────┐
         │ File Discovery │               │ Cube Generator │
         │ (WalkBuilder)  │               │                │
         └───────┬────────┘               └───────┬────────┘
                 │                                │
                 ▼                                ▼
         ┌────────────────────────────────────────────────┐
         │              Pattern Matcher                    │
         │    (matches paths against formatter patterns)   │
         └────────────────────────┬───────────────────────┘
                                  │
                                  ▼
         ┌────────────────────────────────────────────────┐
         │               Format Runner                     │
         │      (groups files by formatter type)           │
         └────────────────────────┬───────────────────────┘
                                  │
                  ┌───────┬───────┼───────┬───────┐
                  ▼       ▼       ▼       ▼       ▼
              rustfmt  nixfmt  alejandra gofmt  cue fmt
```

### Key Components

**File Discovery (`cuenv fmt` only)**
Uses `ignore::WalkBuilder` to walk the project directory, automatically respecting `.gitignore` patterns. This ensures vendor directories, build artifacts, and other ignored files are never formatted.

**Pattern Matcher**
Matches file paths against glob patterns using the `glob` crate. Patterns are matched against relative paths from the project root, supporting recursive wildcards (`**`) and standard glob syntax.

**Format Runner**
Orchestrates formatter execution:
1. Groups files by formatter type (rust, nix, go, cue)
2. Invokes the appropriate tool for each group
3. Handles check vs. fix mode
4. Collects and reports results

**Tool Executors**
Invoke external formatting tools with appropriate flags:

| Formatter | Normal Mode | Check Mode |
|-----------|-------------|------------|
| rustfmt | `rustfmt [--edition X] <files>` | `rustfmt --check <files>` |
| nixfmt | `nixfmt <files>` | `nixfmt --check <files>` |
| alejandra | `alejandra <files>` | `alejandra -c <files>` |
| gofmt | `gofmt -w <files>` | `gofmt -l <files>` |
| cue fmt | `cue fmt <files>` | `cue fmt -d <files>` |

## Configuration Schema

Formatters are defined in `#Base.formatters` (available on both `#Base` and `#Project`):

```cue
#Formatters: {
    rust?: #RustFormatter
    nix?:  #NixFormatter
    go?:   #GoFormatter
    cue?:  #CueFormatter
}
```

Each formatter follows a common structure:

| Field | Type | Description |
|-------|------|-------------|
| `enabled` | `bool` | Enable/disable (default: `true`) |
| `includes` | `[...string]` | Glob patterns to match |

### Formatter Definitions

**RustFormatter**
```cue
#RustFormatter: close({
    enabled:  bool | *true
    includes: [...string] | *["*.rs"]
    edition?: "2018" | "2021" | "2024"
})
```

**NixFormatter**
```cue
#NixFormatter: close({
    enabled:  bool | *true
    includes: [...string] | *["*.nix"]
    tool:     "nixfmt" | "alejandra" | *"nixfmt"
})
```

**GoFormatter**
```cue
#GoFormatter: close({
    enabled:  bool | *true
    includes: [...string] | *["*.go"]
})
```

**CueFormatter**
```cue
#CueFormatter: close({
    enabled:  bool | *true
    includes: [...string] | *["*.cue"]
})
```

## Pattern Matching

Patterns use standard glob syntax and match against relative paths from the project root:

```
project/
├── src/
│   └── lib.rs       → matched as "src/lib.rs"
├── tests/
│   └── test.rs      → matched as "tests/test.rs"
└── build.rs         → matched as "build.rs"
```

**Pattern examples:**

| Pattern | Matches |
|---------|---------|
| `*.rs` | Rust files in project root only |
| `**/*.rs` | All Rust files recursively |
| `src/**/*.rs` | Rust files under src/ |
| `crates/*/src/*.rs` | Rust files in any crate's src directory |

Invalid glob patterns are logged as warnings and skipped, rather than failing the entire operation.

## Execution Modes

### cuenv fmt

Format all files matching configured patterns:

```bash
# Check mode (default) - validate without changes
cuenv fmt

# Fix mode - format files in-place
cuenv fmt --fix

# Filter to specific formatters
cuenv fmt --only rust,go
```

### cuenv sync cubes

Format only generated files after code generation:

```bash
# Normal mode - generate and format
cuenv sync cubes

# Check mode - verify generation and formatting
cuenv sync cubes --check

# Dry run - show what would happen
cuenv sync cubes --dry-run
```

## Exit Codes

| Code | Meaning |
|------|---------|
| `0` | Success (all files formatted or check passed) |
| `3` | Formatting check failed (files need formatting) |

## Error Handling

- **Check mode failures**: Return error and exit non-zero
- **Fix mode failures**: Logged as warnings, execution continues
- **Invalid patterns**: Logged as warnings, pattern skipped
- **Missing tools**: Error with clear message about which tool is missing

## Rust Implementation

The formatter system is implemented in two modules:

**`crates/cuenv/src/commands/fmt.rs`**
Implements `cuenv fmt` with file discovery:

```rust
pub fn execute_fmt(
    path: &str,
    package: &str,
    fix: bool,
    only: Option<&[String]>,
) -> Result<String>
```

**`crates/cuenv/src/commands/sync/formatters.rs`**
Provides formatter runners used by both entry points:

```rust
pub fn run_rust_formatter(files: &[&Path], ...) -> Result<String>
pub fn run_nix_formatter(files: &[&Path], ...) -> Result<String>
pub fn run_go_formatter(files: &[&Path], ...) -> Result<String>
pub fn run_cue_formatter(files: &[&Path], ...) -> Result<String>
pub fn matches_any_pattern(path: &str, patterns: &[String]) -> bool
```

## See Also

- [Formatters How-to Guide](/how-to/formatters/) - Practical usage
- [CLI Reference](/reference/cli/#cuenv-fmt) - Command documentation
- [Cubes](/how-to/cubes/) - Code generation
- [CUE Schema Reference](/reference/cue-schema/) - Complete schema documentation
