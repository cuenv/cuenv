# cuenv-codegen

Code generation and project scaffolding from CUE templates.

## Overview

`cuenv-codegen` provides a code generation system using CUE to generate project files. Features:

- **Schema-wrapped code blocks** - Use `schema.#TypeScript`, `schema.#JSON`, etc. to define files
- **Managed vs Scaffold modes** - Choose whether files are always regenerated or only created once
- **Integrated with cuenv** - Use `cuenv sync codegen` to generate files

## What is CUE Codegen?

**Codegen** is a CUE-based template that generates multiple project files. Define your codegen in your project's `env.cue` file using `schema.#Codegen`.

## Key Concepts

### File Modes

- **Managed**: Files are always regenerated when you run `cuenv sync codegen`. Use for configuration files, CI/CD workflows, etc.
- **Scaffold**: Files are only created if they don't exist. Once created, the user owns them. Use for application code, handlers, services, etc.

### Schema-Based Code

Use CUE schemas to define files with type-safe formatting:

```cue
import "github.com/cuenv/cuenv/schema"

schema.#Project & {
    name: "my-service"

    codegen: {
        files: {
            "src/main.ts": schema.#TypeScript & {
                mode: "scaffold"
                content: """
                    console.log("Hello, world!");
                    """
            }

            "package.json": schema.#JSON & {
                mode: "managed"
                content: """
                    {"name": "my-service", "version": "1.0.0"}
                    """
            }
        }
    }
}
```

## Usage

### CLI

```bash
# Sync codegen files for current project
cuenv sync codegen .

# Sync codegen files for all projects in module
cuenv sync codegen

# Dry run - show what would be generated
cuenv sync codegen --dry-run

# Check if files are in sync
cuenv sync codegen --check
```

### Programmatic (Rust)

```rust
use cuenv_codegen::{Codegen, Generator, GenerateOptions};

let codegen = Codegen::load("my-project/env.cue")?;
let generator = Generator::new(codegen);

let options = GenerateOptions {
    output_dir: PathBuf::from("./my-project"),
    check: false,
    diff: false,
};

generator.generate(&options)?;
```

## Available Schemas

- `schema.#TypeScript` - TypeScript files
- `schema.#JavaScript` - JavaScript files
- `schema.#JSON` - JSON files
- `schema.#JSONC` - JSON with comments
- `schema.#YAML` - YAML files
- `schema.#TOML` - TOML files
- `schema.#Rust` - Rust files
- `schema.#Go` - Go files
- `schema.#Python` - Python files
- `schema.#Markdown` - Markdown files
- `schema.#ShellScript` - Shell scripts
- `schema.#Dockerfile` - Dockerfiles
- `schema.#Nix` - Nix expressions

## License

AGPL-3.0-or-later
