# cuenv-codegen

Literate project scaffolding and management using CUE Cubes.

## Overview

`cuenv-codegen` is a code generation system that uses pure CUE Cubes to generate project files. It features:

- **Schema-wrapped code blocks** - Use `code.#TypeScript`, `code.#Rust`, etc. to define files
- **Managed vs Scaffold modes** - Choose whether files are always regenerated or only created once
- **Auto-generated formatter configs** - Biome, Prettier, rustfmt configs generated from CUE schemas
- **Language-specific formatting** - Built-in support for JSON, TypeScript, Rust, and more

## What is a CUE Cube?

A **Cube** is a CUE-based template that generates multiple project files. Think of it as a 3D blueprint - each face of the cube represents different aspects of your project (source code, config, tests, etc.)

## Key Concepts

### File Modes

- **Managed**: Files are always regenerated when you run codegen. These are typically configuration files, CI/CD workflows, etc.
- **Scaffold**: Files are only created if they don't exist. Once created, the user owns them. Perfect for application code, handlers, services, etc.

### Schema-Based Code

Instead of string templates, use CUE schemas to define code:

```cue
import "github.com/cuenv/cuenv-codegen/schemas/code"

files: {
    "src/main.ts": code.#TypeScript & {
        mode: "scaffold"
        format: {
            indent: "space"
            indentSize: 2
            quotes: "single"
        }
        content: """
            import express from 'express';

            const app = express();
            app.listen(3000);
            """
    }
}
```

### Format Configuration

Format settings are embedded in the schema and can be used to auto-generate formatter configs:

```cue
files: {
    "package.json": code.#JSON & {
        mode: "managed"
        format: {
            indent: "space"
            indentSize: 2
        }
        content: json.Marshal({...})
    }
}
```

## Example Cube

See `examples/simple-api.cue` for a complete example of a Node.js API cube that:

- Generates `package.json` with conditional dependencies
- Creates TypeScript source files
- Generates `tsconfig.json`
- Conditionally includes database service files

## Usage

```rust
use cuenv_codegen::{Cube, Generator, GenerateOptions};

let cube = Cube::load("my-project.cube.cue")?;
let generator = Generator::new(cube);

let options = GenerateOptions {
    output_dir: PathBuf::from("./my-project"),
    check: false,
    diff: false,
};

generator.generate(&options)?;
```

## Status

This is an initial implementation (Phase 1) with:

- CUE Cube loading
- File generation with managed/scaffold modes
- JSON formatting
- Config generation for biome, prettier, rustfmt
- CUE evaluation (currently requires pre-evaluated JSON)
- TypeScript/Rust formatting integration
- CLI tool
- VSCode plugin

## Future Phases

- **Phase 2**: Config generation improvements
- **Phase 3**: VSCode plugin for syntax highlighting
- **Phase 4**: Full LSP integration
- **Phase 5**: Cube registry
- **Phase 6**: Advanced features (diff mode, composition, etc.)

## License

AGPL-3.0-or-later
