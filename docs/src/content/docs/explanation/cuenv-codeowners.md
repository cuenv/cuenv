---
title: cuenv-codeowners
description: Generate CODEOWNERS files with configurable section styles
---

The `cuenv-codeowners` crate provides a builder-based API for generating CODEOWNERS files that define code ownership rules for your repository. It is provider-agnostic; platform-specific logic (paths, section styles) belongs in provider crates like `cuenv-github` or `cuenv-gitlab`.

## Overview

CODEOWNERS files automatically assign reviewers to pull requests based on file paths. This crate lets you programmatically generate these files with:

- Configurable section styles (comment `#` or bracket `[]`)
- Section grouping for organized output
- Custom headers and descriptions

## Architecture

```text
┌────────────────────┐     ┌─────────────────┐     ┌──────────────────┐
│ CodeOwnersBuilder  │────►│   CodeOwners    │────►│  .CODEOWNERS     │
│ (fluent API)       │     │  (config)       │     │  (output file)   │
└────────────────────┘     └─────────────────┘     └──────────────────┘
                                   │
                                   ▼
                           ┌─────────────────┐
                           │  SectionStyle   │
                           │ (formatting)    │
                           └─────────────────┘
```

### Key Components

**SectionStyle**
Enum representing how sections are formatted in the output.

**Rule**
A single ownership rule mapping a file pattern to one or more owners.

**CodeOwners**
The main configuration struct holding section style, rules, and output settings.

**CodeOwnersBuilder**
Fluent builder for constructing `CodeOwners` configurations.

## API Reference

### SectionStyle

Section formatting style for CODEOWNERS files:

```rust
use cuenv_codeowners::SectionStyle;

// Each style formats sections differently
let comment = SectionStyle::Comment;  // # Section Name (GitHub, Bitbucket)
let bracket = SectionStyle::Bracket;  // [Section Name] (GitLab)
let none = SectionStyle::None;        // No section headers
```

| Style     | Output          | Used By              |
| --------- | --------------- | -------------------- |
| `Comment` | `# Section`     | GitHub, Bitbucket    |
| `Bracket` | `[Section]`     | GitLab               |
| `None`    | (no headers)    | Custom configurations|

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

| Method                 | Description                               |
| ---------------------- | ----------------------------------------- |
| `new(pattern, owners)` | Create a rule with pattern and owner list |
| `description(text)`    | Add a comment above the rule              |
| `section(name)`        | Assign to a section for grouped output    |

### CodeOwners

Main configuration and generator:

```rust
use cuenv_codeowners::{CodeOwners, SectionStyle, Rule};

let codeowners = CodeOwners::builder()
    .section_style(SectionStyle::Comment)
    .header("Code ownership rules")
    .rule(Rule::new("*", ["@org/maintainers"]))
    .rule(Rule::new("*.rs", ["@rust-team"]))
    .rule(Rule::new("/docs/**", ["@docs-team"]).section("Documentation"))
    .build();

// Generate file content
let content = codeowners.generate();
```

| Method       | Description                          |
| ------------ | ------------------------------------ |
| `builder()`  | Create a new builder                 |
| `generate()` | Generate the CODEOWNERS file content |

### CodeOwnersBuilder

Fluent builder for configuration:

| Method                    | Description                          |
| ------------------------- | ------------------------------------ |
| `section_style(style)`    | Set section formatting style         |
| `header(str)`             | Set header comment                   |
| `rule(Rule)`              | Add a single rule                    |
| `rules(iter)`             | Add multiple rules                   |
| `build()`                 | Build the `CodeOwners` configuration |

## Features

The crate supports optional features:

| Feature | Description                          |
| ------- | ------------------------------------ |
| `serde` | Enable serialization/deserialization |

```toml
[dependencies]
cuenv-codeowners = { version = "...", features = ["serde"] }
```

## Integration Patterns

### Basic Usage

```rust
use cuenv_codeowners::{CodeOwners, SectionStyle, Rule};
use std::fs;

let codeowners = CodeOwners::builder()
    .section_style(SectionStyle::Comment)
    .rule(Rule::new("*", ["@org/core-team"]))
    .rule(Rule::new("*.rs", ["@rust-team"]))
    .rule(Rule::new("*.ts", ["@frontend-team"]))
    .build();

let content = codeowners.generate();
fs::write(".github/CODEOWNERS", content)?;
```

### Organized Sections

```rust
use cuenv_codeowners::{CodeOwners, SectionStyle, Rule};

let codeowners = CodeOwners::builder()
    .section_style(SectionStyle::Comment)
    .header("Auto-generated CODEOWNERS\nDo not edit manually")
    // Rules with same section are grouped together
    .rule(Rule::new("*", ["@org/maintainers"]))
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

### GitLab Bracket Sections

GitLab uses `[Section]` syntax instead of `# Section`:

```rust
use cuenv_codeowners::{CodeOwners, SectionStyle, Rule};

let codeowners = CodeOwners::builder()
    .section_style(SectionStyle::Bracket)
    .rule(Rule::new("*.rs", ["@backend"]).section("Backend"))
    .build();

println!("{}", codeowners.generate());
// Output:
// [Backend]
// *.rs @backend
```

## Provider Crates

For platform-specific CODEOWNERS management (paths, detection), use the provider crates:

- `cuenv-github` - GitHub CODEOWNERS (`.github/CODEOWNERS`, comment sections)
- `cuenv-gitlab` - GitLab CODEOWNERS (`CODEOWNERS`, bracket sections)
- `cuenv-bitbucket` - Bitbucket CODEOWNERS (`CODEOWNERS`, comment sections)

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
