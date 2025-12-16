---
title: cuenv-cubes (Codegen)
description: Code generation and project scaffolding from CUE templates
---

The `cuenv-cubes` crate provides a code generation system using CUE templates to generate multiple project files. It supports both managed (regenerated) and scaffolded (created once) file modes with language-specific formatting.

## Overview

Cubes enable you to:

- Generate project files from CUE configuration
- Use language-specific schemas with formatting defaults
- Choose between managed (always regenerated) and scaffold (create once) modes
- Integrate file generation with gitignore management
- Preview changes with dry-run mode

## Architecture

```text
┌────────────────────┐     ┌─────────────────┐     ┌──────────────────┐
│  env.cue           │────►│      Cube       │────►│  Generated Files │
│  (cube config)     │     │   (evaluator)   │     │  (src/*, etc.)   │
└────────────────────┘     └─────────────────┘     └──────────────────┘
                                   │
                                   ▼
                           ┌─────────────────┐
                           │    Generator    │
                           │ (file writer)   │
                           └─────────────────┘
```

### Key Components

**Cube**
CUE configuration containing file definitions and optional context data.

**ProjectFile**
A file definition with content, language, mode, and formatting options.

**FileMode**
Determines whether a file is managed (always regenerated) or scaffolded (created once).

**Generator**
Handles file writing, status tracking, and sync operations.

## Schema Reference

### #Cube

The top-level cube configuration:

```cue
#Cube: {
    files:    [string]: cubes.#ProjectFile  // Files to generate (path → definition)
    context?: _                              // Optional context data for templating
}
```

| Field     | Type                      | Description                           |
| --------- | ------------------------- | ------------------------------------- |
| `files`   | `map[string]#ProjectFile` | Map of file paths to file definitions |
| `context` | `any` (optional)          | Context data available for templating |

### #ProjectFile

Base schema for all file definitions:

```cue
#ProjectFile: {
    content:    string
    language:   string
    mode:       "managed" | "scaffold" | *"managed"
    gitignore:  bool  // Defaults based on mode
    format?:    #FormatConfig
    lint?:      #LintConfig
}
```

| Field       | Type            | Default     | Description                        |
| ----------- | --------------- | ----------- | ---------------------------------- |
| `content`   | `string`        | required    | The file content to generate       |
| `language`  | `string`        | required    | Language identifier for formatting |
| `mode`      | `string`        | `"managed"` | Generation mode                    |
| `gitignore` | `bool`          | mode-based  | Whether to add to `.gitignore`     |
| `format`    | `#FormatConfig` | optional    | Formatting configuration           |
| `lint`      | `#LintConfig`   | optional    | Linting configuration              |

### File Modes

| Mode       | Description                                      | gitignore Default |
| ---------- | ------------------------------------------------ | ----------------- |
| `managed`  | Always regenerated when `cuenv sync cubes` runs  | `true`            |
| `scaffold` | Only created if file doesn't exist; user owns it | `false`           |

### #FormatConfig

Formatting options available for all file types:

```cue
format: {
    indent:         "space" | "tab"
    indentSize?:    int & >=1 & <=8
    lineWidth?:     int & >=60 & <=200
    trailingComma?: "none" | "all" | "es5"
    semicolons?:    bool
    quotes?:        "single" | "double"
}
```

## Language Schemas

Use language-specific schemas for type-safe formatting defaults:

### TypeScript / JavaScript

```cue
import "github.com/cuenv/cuenv/schema/cubes"

cubes.#TypeScriptFile & {
    content: """
        export const greeting = "Hello, world!";
        """
}
```

| Field                  | Default    | Description             |
| ---------------------- | ---------- | ----------------------- |
| `format.indent`        | `"space"`  | Indentation character   |
| `format.indentSize`    | `2`        | Spaces per indent level |
| `format.lineWidth`     | `100`      | Maximum line width      |
| `format.trailingComma` | `"all"`    | Trailing comma style    |
| `format.semicolons`    | `true`     | Include semicolons      |
| `format.quotes`        | `"double"` | Quote style             |

### Rust

```cue
cubes.#RustFile & {
    content: """
        fn main() {
            println!("Hello, world!");
        }
        """
}
```

| Field               | Default   | Description                   |
| ------------------- | --------- | ----------------------------- |
| `format.indent`     | `"space"` | Indentation (Rust convention) |
| `format.indentSize` | `4`       | Spaces per indent level       |
| `format.lineWidth`  | `100`     | Maximum line width            |
| `rustfmt.edition`   | `"2021"`  | Rust edition                  |

### Go

```cue
cubes.#GoFile & {
    content: """
        package main

        func main() {
            fmt.Println("Hello, world!")
        }
        """
}
```

| Field               | Default | Description         |
| ------------------- | ------- | ------------------- |
| `format.indent`     | `"tab"` | Go convention: tabs |
| `format.indentSize` | `8`     | Tab width           |

### Python

```cue
cubes.#PythonFile & {
    content: """
        def main():
            print("Hello, world!")
        """
}
```

| Field               | Default   | Description             |
| ------------------- | --------- | ----------------------- |
| `format.indent`     | `"space"` | PEP 8 convention        |
| `format.indentSize` | `4`       | Spaces per indent level |
| `format.lineWidth`  | `88`      | Black formatter default |

### JSON / JSONC

```cue
cubes.#JSONFile & {
    content: """
        {"name": "my-project", "version": "1.0.0"}
        """
}
```

Use `#JSONCFile` for JSON with comments (e.g., `tsconfig.json`).

### YAML / TOML

```cue
cubes.#YAMLFile & {
    content: """
        name: my-project
        version: 1.0.0
        """
}
```

### All Available Schemas

| Schema             | Language     | Indent Default | Notes                     |
| ------------------ | ------------ | -------------- | ------------------------- |
| `#TypeScriptFile`  | `typescript` | 2 spaces       | Includes tsconfig options |
| `#JavaScriptFile`  | `javascript` | 2 spaces       | ES formatting defaults    |
| `#JSONFile`        | `json`       | 2 spaces       | Strict JSON               |
| `#JSONCFile`       | `jsonc`      | 2 spaces       | JSON with comments        |
| `#YAMLFile`        | `yaml`       | 2 spaces       | YAML files                |
| `#TOMLFile`        | `toml`       | 2 spaces       | TOML files                |
| `#RustFile`        | `rust`       | 4 spaces       | Includes rustfmt options  |
| `#GoFile`          | `go`         | tabs           | Go convention             |
| `#PythonFile`      | `python`     | 4 spaces       | Black-compatible defaults |
| `#MarkdownFile`    | `markdown`   | 2 spaces       | Documentation             |
| `#ShellScriptFile` | `shell`      | 2 spaces       | Shell scripts             |
| `#DockerfileFile`  | `dockerfile` | 4 spaces       | Dockerfiles               |
| `#NixFile`         | `nix`        | 2 spaces       | Nix expressions           |

## Rust API Reference

### Cube

Load and evaluate cube configuration:

```rust
use cuenv_cubes::Cube;

let cube = Cube::load("my-project/env.cue")?;
```

### Generator

Generate files from cube configuration:

```rust
use cuenv_cubes::{Generator, GenerateOptions};
use std::path::PathBuf;

let generator = Generator::new(cube);

let options = GenerateOptions {
    output_dir: PathBuf::from("./my-project"),
    check: false,  // Set true to verify without writing
    diff: false,   // Set true to show diffs
};

let result = generator.generate(&options)?;

for file in &result.files {
    println!("{}: {}", file.status, file.path.display());
}
```

### FileStatus

Track what happened to each file:

```rust
use cuenv_cubes::FileStatus;

match status {
    FileStatus::Created => println!("New file created"),
    FileStatus::Updated => println!("File updated"),
    FileStatus::Unchanged => println!("No changes needed"),
    FileStatus::Skipped => println!("Scaffold file exists, skipped"),
    FileStatus::WouldCreate => println!("Would create (dry-run/check)"),
    FileStatus::WouldUpdate => println!("Would update (dry-run/check)"),
}
```

## Integration Patterns

### Basic Project Setup

```cue
import "github.com/cuenv/cuenv/schema"
import "github.com/cuenv/cuenv/schema/cubes"

schema.#Project & {
    name: "my-service"

    cube: {
        files: {
            "package.json": cubes.#JSONFile & {
                mode: "managed"
                content: """
                    {
                      "name": "my-service",
                      "version": "1.0.0",
                      "type": "module"
                    }
                    """
            }

            "src/index.ts": cubes.#TypeScriptFile & {
                mode: "scaffold"
                content: """
                    console.log("Hello, world!");
                    """
            }
        }
    }
}
```

### Using Context for Templating

```cue
schema.#Project & {
    name: "my-service"

    cube: {
        context: {
            serviceName: "users"
            port: 3000
        }

        files: {
            "src/config.ts": cubes.#TypeScriptFile & {
                content: """
                    export const config = {
                      serviceName: "\(context.serviceName)",
                      port: \(context.port),
                    };
                    """
            }
        }
    }
}
```

### gitignore Integration

Files with `gitignore: true` are automatically added to `.gitignore`:

```cue
cube: {
    files: {
        // Managed files default to gitignore: true
        "dist/bundle.js": cubes.#JavaScriptFile & {
            mode: "managed"  // gitignore: true by default
            content: "..."
        }

        // Scaffold files default to gitignore: false
        "src/main.ts": cubes.#TypeScriptFile & {
            mode: "scaffold"  // gitignore: false by default
            content: "..."
        }

        // Override defaults explicitly
        "generated/types.ts": cubes.#TypeScriptFile & {
            mode: "managed"
            gitignore: false  // Commit this generated file
            content: "..."
        }
    }
}
```

## Features

The crate uses serde for serialization/deserialization.

```toml
[dependencies]
cuenv-cubes = "..."
```

## Testing

```bash
# Run all cubes tests
cargo test -p cuenv-cubes

# Run with features
cargo test -p cuenv-cubes --features serde
```

## See Also

- [How-to: Cubes](/how-to/cubes/) - Using cubes with cuenv
- [cuenv-ignore](/explanation/cuenv-ignore/) - Generate ignore files
- [cuenv-codeowners](/explanation/cuenv-codeowners/) - Generate CODEOWNERS files
- [API Reference](/reference/rust-api/) - Complete API documentation
