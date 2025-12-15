# cuenv-cubes

CUE Cubes - code generation and project scaffolding from CUE templates.

## Overview

`cuenv-cubes` provides a code generation system using CUE Cubes to generate project files. Features:

- **Schema-wrapped code blocks** - Use `schema.#TypeScript`, `schema.#JSON`, etc. to define files
- **Managed vs Scaffold modes** - Choose whether files are always regenerated or only created once
- **Integrated with cuenv** - Use `cuenv sync cubes` to generate files

## What is a CUE Cube?

A **Cube** is a CUE-based template that generates multiple project files. Define your cube in your project's `env.cue` file using `schema.#Cube`.

## Key Concepts

### File Modes

- **Managed**: Files are always regenerated when you run `cuenv sync cubes`. Use for configuration files, CI/CD workflows, etc.
- **Scaffold**: Files are only created if they don't exist. Once created, the user owns them. Use for application code, handlers, services, etc.

### Schema-Based Code

Use CUE schemas to define files with type-safe formatting:

```cue
import "github.com/cuenv/cuenv/schema"

schema.#Project & {
    name: "my-service"

    cube: {
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
# Sync cube files for current project
cuenv sync cubes .

# Sync cube files for all projects in module
cuenv sync cubes

# Dry run - show what would be generated
cuenv sync cubes --dry-run

# Check if files are in sync
cuenv sync cubes --check
```

### Programmatic (Rust)

```rust
use cuenv_cubes::{Cube, Generator, GenerateOptions};

let cube = Cube::load("my-project/env.cue")?;
let generator = Generator::new(cube);

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
