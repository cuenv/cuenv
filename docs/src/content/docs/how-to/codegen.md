---
title: Codegen
description: Code generation and project scaffolding from CUE templates with cuenv
---

cuenv can generate and manage project files from CUE templates using codegen. This enables you to define configuration files, scaffolding code, and generated assets in a type-safe, declarative way.

## Quick Start

Add a `codegen` field to your `env.cue`:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"
import gen "github.com/cuenv/cuenv/schema/codegen"

schema.#Project & {
    name: "my-project"

    codegen: {
        files: {
            "package.json": gen.#JSONFile & {
                mode: "managed"
                content: """
                    {
                      "name": "my-project",
                      "version": "1.0.0"
                    }
                    """
            }

            "src/index.ts": gen.#TypeScriptFile & {
                mode: "scaffold"
                content: """
                    console.log("Hello, world!");
                    """
            }
        }
    }
}
```

Then run:

```bash
cuenv sync
```

This generates the files defined in your codegen configuration.

## Commands

### Generate all codegen files

```bash
cuenv sync
```

Runs all sync operations, including codegen file generation.

### Generate only codegen files

```bash
cuenv sync codegen
```

Runs only the codegen file generation.

### Generate for a specific project

```bash
cuenv sync codegen .
```

Sync codegen files for the current directory's project only.

### Preview changes (dry-run)

```bash
cuenv sync --dry-run
```

Shows what files would be created or updated without actually writing them.

### Check if files are in sync

```bash
cuenv sync --check
```

Verifies generated files match the configuration. Useful in CI pipelines.

## File Modes

Codegen supports two file modes that determine how files are generated:

### Managed Mode

Files in managed mode are **always regenerated** when you run `cuenv sync codegen`. Use this for:

- Configuration files (`package.json`, `tsconfig.json`, etc.)
- CI/CD workflows (`.github/workflows/*.yml`)
- Generated code that should always match the source of truth

```cue
"package.json": gen.#JSONFile & {
    mode: "managed"  // Always regenerated
    content: """
        {"name": "my-project"}
        """
}
```

**Note:** Managed files default to `gitignore: true` since they're generated from CUE.

### Scaffold Mode

Files in scaffold mode are **only created if they don't exist**. Once created, you own them. Use this for:

- Application entry points
- Handler functions
- Service implementations
- Any code the developer will modify

```cue
"src/main.ts": gen.#TypeScriptFile & {
    mode: "scaffold"  // Only created if missing
    content: """
        console.log("Hello, world!");
        """
}
```

**Note:** Scaffold files default to `gitignore: false` since they become user-owned.

## Available Schemas

Use language-specific schemas for type-safe formatting:

| Schema             | Use For               | Indent Default |
| ------------------ | --------------------- | -------------- |
| `#TypeScriptFile`  | `.ts`, `.tsx` files   | 2 spaces       |
| `#JavaScriptFile`  | `.js`, `.jsx` files   | 2 spaces       |
| `#JSONFile`        | `.json` files         | 2 spaces       |
| `#JSONCFile`       | JSON with comments    | 2 spaces       |
| `#YAMLFile`        | `.yaml`, `.yml` files | 2 spaces       |
| `#TOMLFile`        | `.toml` files         | 2 spaces       |
| `#RustFile`        | `.rs` files           | 4 spaces       |
| `#GoFile`          | `.go` files           | tabs           |
| `#PythonFile`      | `.py` files           | 4 spaces       |
| `#MarkdownFile`    | `.md` files           | 2 spaces       |
| `#ShellScriptFile` | `.sh` files           | 2 spaces       |
| `#DockerfileFile`  | Dockerfiles           | 4 spaces       |
| `#NixFile`         | `.nix` files          | 2 spaces       |

Import schemas from `github.com/cuenv/cuenv/schema/codegen`.

## Format Configuration

Override default formatting options:

```cue
"src/app.ts": gen.#TypeScriptFile & {
    format: {
        indent:        "space"
        indentSize:    4         // Override default 2
        lineWidth:     120       // Override default 100
        trailingComma: "es5"     // Override default "all"
        semicolons:    false     // Override default true
        quotes:        "single"  // Override default "double"
    }
    content: """
        const app = 'hello'
        """
}
```

## gitignore Integration

Codegen files can automatically be added to `.gitignore`:

```cue
codegen: {
    files: {
        // Managed files: gitignore defaults to true
        "dist/output.js": gen.#JavaScriptFile & {
            mode: "managed"
            // gitignore: true (default for managed)
            content: "..."
        }

        // Scaffold files: gitignore defaults to false
        "src/handler.ts": gen.#TypeScriptFile & {
            mode: "scaffold"
            // gitignore: false (default for scaffold)
            content: "..."
        }

        // Override explicitly when needed
        "generated/api-types.ts": gen.#TypeScriptFile & {
            mode: "managed"
            gitignore: false  // Commit this generated file
            content: "..."
        }
    }
}
```

When you run `cuenv sync`, files marked with `gitignore: true` are automatically added to your `.gitignore` under a "Codegen-generated files" section.

## Using Context

Pass configuration data to your codegen templates:

```cue
schema.#Project & {
    name: "api-service"

    codegen: {
        context: {
            serviceName: "users"
            port:        3000
            features: ["auth", "logging"]
        }

        files: {
            "src/config.ts": gen.#TypeScriptFile & {
                content: """
                    export const config = {
                      serviceName: "\(context.serviceName)",
                      port: \(context.port),
                      features: \(context.features),
                    };
                    """
            }
        }
    }
}
```

## Output Status

When running `cuenv sync`, you'll see the status of each file:

- **Created** - New file was created
- **Updated** - Existing file was updated with new content
- **Unchanged** - File exists and content matches (no write needed)
- **Skipped** - Scaffold file already exists (not overwritten)

In dry-run mode:

- **Would create** - File would be created
- **Would update** - File would be updated

## Examples

### TypeScript Project

```cue
import "github.com/cuenv/cuenv/schema"
import gen "github.com/cuenv/cuenv/schema/codegen"

schema.#Project & {
    name: "my-ts-app"

    codegen: {
        files: {
            "package.json": gen.#JSONFile & {
                mode: "managed"
                content: """
                    {
                      "name": "my-ts-app",
                      "version": "1.0.0",
                      "type": "module",
                      "scripts": {
                        "build": "tsc",
                        "start": "node dist/index.js"
                      }
                    }
                    """
            }

            "tsconfig.json": gen.#JSONCFile & {
                mode: "managed"
                content: """
                    {
                      "compilerOptions": {
                        "target": "ES2022",
                        "module": "NodeNext",
                        "outDir": "dist",
                        "strict": true
                      },
                      "include": ["src"]
                    }
                    """
            }

            "src/index.ts": gen.#TypeScriptFile & {
                mode: "scaffold"
                content: """
                    console.log("Hello, world!");
                    """
            }
        }
    }
}
```

### Rust Project

```cue
import "github.com/cuenv/cuenv/schema"
import gen "github.com/cuenv/cuenv/schema/codegen"

schema.#Project & {
    name: "my-rust-app"

    codegen: {
        files: {
            "Cargo.toml": gen.#TOMLFile & {
                mode: "managed"
                content: """
                    [package]
                    name = "my-rust-app"
                    version = "0.1.0"
                    edition = "2021"

                    [dependencies]
                    """
            }

            "src/main.rs": gen.#RustFile & {
                mode: "scaffold"
                content: """
                    fn main() {
                        println!("Hello, world!");
                    }
                    """
            }
        }
    }
}
```

### Multi-Service Monorepo

```cue
import "github.com/cuenv/cuenv/schema"
import gen "github.com/cuenv/cuenv/schema/codegen"

let services = ["auth", "users", "billing"]

schema.#Project & {
    name: "platform"

    codegen: {
        files: {
            for svc in services {
                "services/\(svc)/package.json": gen.#JSONFile & {
                    mode: "managed"
                    content: """
                        {
                          "name": "@platform/\(svc)",
                          "version": "1.0.0"
                        }
                        """
                }

                "services/\(svc)/src/index.ts": gen.#TypeScriptFile & {
                    mode: "scaffold"
                    content: """
                        // \(svc) service entry point
                        export async function handler() {
                          // TODO: Implement \(svc) logic
                        }
                        """
                }
            }
        }
    }
}
```

### GitHub Actions Workflow

```cue
import "github.com/cuenv/cuenv/schema"
import gen "github.com/cuenv/cuenv/schema/codegen"

schema.#Project & {
    name: "my-project"

    codegen: {
        files: {
            ".github/workflows/ci.yml": gen.#YAMLFile & {
                mode: "managed"
                content: """
                    name: CI
                    on:
                      push:
                        branches: [main]
                      pull_request:
                        branches: [main]

                    jobs:
                      build:
                        runs-on: ubuntu-latest
                        steps:
                          - uses: actions/checkout@v4
                          - uses: actions/setup-node@v4
                          - run: npm ci
                          - run: npm test
                    """
            }
        }
    }
}
```

## Generated File Headers

Managed files can optionally include a header comment indicating they're generated. This helps prevent accidental manual edits:

```typescript
// Generated by cuenv - do not edit
// Source: env.cue

export const config = { ... };
```
