---
title: cuenv-codeowners
description: Generate CODEOWNERS files for GitHub, GitLab, and Bitbucket
---

The `cuenv-codeowners` crate provides a builder-based API for generating CODEOWNERS files that define code ownership rules for your repository. It supports GitHub, GitLab, and Bitbucket with platform-specific syntax.

## Overview

CODEOWNERS files automatically assign reviewers to pull requests based on file paths. This crate lets you programmatically generate these files with:

- Multi-platform support (GitHub, GitLab, Bitbucket)
- Section grouping for organized output
- Default owner rules
- Custom headers and descriptions

## Architecture

```text
┌────────────────────┐     ┌─────────────────┐     ┌──────────────────┐
│ CodeownersBuilder  │────►│   Codeowners    │────►│  .CODEOWNERS     │
│ (fluent API)       │     │  (config)       │     │  (output file)   │
└────────────────────┘     └─────────────────┘     └──────────────────┘
                                   │
                                   ▼
                           ┌─────────────────┐
                           │    Platform     │
                           │ (syntax rules)  │
                           └─────────────────┘
```

### Key Components

**Platform**
Enum representing the target platform (GitHub, GitLab, Bitbucket) with platform-specific syntax rules.

**Rule**
A single ownership rule mapping a file pattern to one or more owners.

**Codeowners**
The main configuration struct holding platform, rules, and output settings.

**CodeownersBuilder**
Fluent builder for constructing `Codeowners` configurations.

## API Reference

### Platform

Target platform for CODEOWNERS file generation:

```rust
use cuenv_codeowners::Platform;

// Each platform has different section syntax and default paths
let github = Platform::Github;    // .github/CODEOWNERS, # Section
let gitlab = Platform::Gitlab;    // CODEOWNERS, [Section]
let bitbucket = Platform::Bitbucket; // CODEOWNERS, # Section
```

| Platform   | Default Path          | Section Syntax |
| ---------- | --------------------- | -------------- |
| `Github`   | `.github/CODEOWNERS`  | `# Section`    |
| `Gitlab`   | `CODEOWNERS`          | `[Section]`    |
| `Bitbucket`| `CODEOWNERS`          | `# Section`    |

### Rule

A single code ownership rule:

```rust
use cuenv_codeowners::Rule;

// Basic rule
let rule = Rule::new("*.rs", ["@rust-team"]);

// Rule with description and section
let rule = Rule::new("/docs/**", ["@docs-team", "@tech-writers"])
    .description("Documentation files")
    .section("Documentation");
```

| Method                  | Description                                    |
| ----------------------- | ---------------------------------------------- |
| `new(pattern, owners)`  | Create a rule with pattern and owner list      |
| `description(text)`     | Add a comment above the rule                   |
| `section(name)`         | Assign to a section for grouped output         |

### Codeowners

Main configuration and generator:

```rust
use cuenv_codeowners::{Codeowners, Platform, Rule};

let codeowners = Codeowners::builder()
    .platform(Platform::Github)
    .header("Code ownership rules")
    .default_owners(["@org/maintainers"])
    .rule(Rule::new("*.rs", ["@rust-team"]))
    .rule(Rule::new("/docs/**", ["@docs-team"]).section("Documentation"))
    .build();

// Generate file content
let content = codeowners.generate();

// Get output path
let path = codeowners.output_path(); // ".github/CODEOWNERS"
```

| Method                        | Description                                      |
| ----------------------------- | ------------------------------------------------ |
| `builder()`                   | Create a new builder                             |
| `generate()`                  | Generate the CODEOWNERS file content             |
| `output_path()`               | Get the output path (custom or platform default) |
| `detect_platform(repo_root)`  | Auto-detect platform from repo structure         |

### CodeownersBuilder

Fluent builder for configuration:

| Method                  | Description                                       |
| ----------------------- | ------------------------------------------------- |
| `platform(Platform)`    | Set target platform                               |
| `path(str)`             | Override output path                              |
| `header(str)`           | Set header comment                                |
| `default_owners(iter)`  | Set default owners (`*` rule)                     |
| `rule(Rule)`            | Add a single rule                                 |
| `rules(iter)`           | Add multiple rules                                |
| `build()`               | Build the `Codeowners` configuration              |

## Features

The crate supports optional features:

| Feature    | Description                                    |
| ---------- | ---------------------------------------------- |
| `serde`    | Enable serialization/deserialization           |
| `schemars` | Enable JSON Schema generation (implies serde)  |

```toml
[dependencies]
cuenv-codeowners = { version = "...", features = ["serde"] }
```

## Integration Patterns

### Basic Usage

```rust
use cuenv_codeowners::{Codeowners, Platform, Rule};
use std::fs;

let codeowners = Codeowners::builder()
    .platform(Platform::Github)
    .default_owners(["@org/core-team"])
    .rule(Rule::new("*.rs", ["@rust-team"]))
    .rule(Rule::new("*.ts", ["@frontend-team"]))
    .build();

let content = codeowners.generate();
fs::write(codeowners.output_path(), content)?;
```

### Platform Auto-Detection

```rust
use cuenv_codeowners::{Codeowners, Platform};
use std::path::Path;

// Detect from repo structure
let platform = Codeowners::detect_platform(Path::new("."));
// Checks for: .github/ -> GitHub, .gitlab-ci.yml -> GitLab, etc.

let codeowners = Codeowners::builder()
    .platform(platform)
    .build();
```

### Organized Sections

```rust
use cuenv_codeowners::{Codeowners, Platform, Rule};

let codeowners = Codeowners::builder()
    .platform(Platform::Github)
    .header("Auto-generated CODEOWNERS\nDo not edit manually")
    .default_owners(["@org/maintainers"])
    // Rules with same section are grouped together
    .rule(Rule::new("*.rs", ["@backend"]).section("Backend"))
    .rule(Rule::new("*.go", ["@backend"]).section("Backend"))
    .rule(Rule::new("*.ts", ["@frontend"]).section("Frontend"))
    .rule(Rule::new("*.tsx", ["@frontend"]).section("Frontend"))
    .rule(Rule::new("/docs/**", ["@docs-team"]).section("Documentation"))
    .build();

println!("{}", codeowners.generate());
// Output:
// # Auto-generated CODEOWNERS
// # Do not edit manually
//
// # Default owners for all files
// * @org/maintainers
//
// # Backend
// *.rs @backend
// *.go @backend
//
// # Documentation
// /docs/** @docs-team
//
// # Frontend
// *.ts @frontend
// *.tsx @frontend
```

### GitLab Sections

GitLab uses `[Section]` syntax instead of `# Section`:

```rust
use cuenv_codeowners::{Codeowners, Platform, Rule};

let codeowners = Codeowners::builder()
    .platform(Platform::Gitlab)
    .rule(Rule::new("*.rs", ["@backend"]).section("Backend"))
    .build();

println!("{}", codeowners.generate());
// Output:
// [Backend]
// *.rs @backend
```

## Testing

```bash
# Run all codeowners tests
cargo test -p cuenv-codeowners

# Run with features
cargo test -p cuenv-codeowners --features serde
```

## See Also

- [cuenv-ignore](/explanation/cuenv-ignore/) - Generate ignore files
- [API Reference](/reference/rust-api/) - Complete API documentation
