---
title: Examples
description: Practical examples and usage patterns for cuenv
---

This page provides practical examples to help you get started with cuenv. Each example demonstrates common patterns you can adapt for your projects.

## Basic Environment Variables

The simplest cuenv configuration defines environment variables with type safety:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
  name: "my-project"
}

env: {
    // Basic string values
    DATABASE_URL: "postgres://localhost/mydb"
    APP_NAME:     "my-application"

    // Boolean and numeric values
    DEBUG: true
    PORT:  3000

    // String interpolation
    BASE_URL:     "https://api.example.com"
    API_ENDPOINT: "\(BASE_URL)/v1"
}
```

**Usage:**

```bash
# Print environment variables
cuenv env print

# Execute a command with these variables
cuenv exec -- bun server.js
```

## Basic Tasks

Define tasks to automate common workflows:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
  name: "my-project"
}

env: {
    NAME: "Developer"
}

tasks: {
    // Simple command with arguments
    greet: {
        command: "echo"
        args: ["Hello", env.NAME, "!"]
    }

    // Task that uses environment variables
    show_env: {
        command: "printenv"
        args: ["NAME"]
    }

    // Shell-specific task
    shell_example: {
        shell: schema.#Bash
        command: "echo"
        args: ["Running in Bash"]
    }
}
```

**Usage:**

```bash
# List available tasks
cuenv task

# Run a specific task
cuenv task greet
```

## Sequential Tasks

Run tasks in a specific order using arrays:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
  name: "my-project"
}

env: {
    PROJECT: "my-app"
}

tasks: {
    // Array of tasks runs sequentially
    deploy: [
        {
            command: "echo"
            args: ["Step 1: Building \(env.PROJECT)..."]
        },
        {
            command: "echo"
            args: ["Step 2: Running tests..."]
        },
        {
            command: "echo"
            args: ["Step 3: Deploying..."]
        },
    ]
}
```

## Nested Task Groups

Organize related tasks into groups:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
  name: "my-project"
}

tasks: {
    // Nested tasks - run with cuenv task database.migrate
    database: {
        migrate: {
            command: "migrate"
            args: ["up"]
        }
        seed: {
            command: "seed"
            args: ["--env", "development"]
        }
        reset: {
            command: "migrate"
            args: ["reset"]
        }
    }

    // Another group
    test: {
        unit: {
            command: "cargo"
            args: ["test", "--lib"]
        }
        integration: {
            command: "cargo"
            args: ["test", "--test", "integration"]
        }
    }
}
```

**Usage:**

```bash
cuenv task database.migrate
cuenv task test.unit
```

## Shell Hooks

Execute commands automatically when entering a directory:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
  name: "my-project"
}

env: {
    CUENV_TEST:   "loaded_successfully"
    API_ENDPOINT: "http://localhost:8080/api"
    DEBUG_MODE:   "true"
    PROJECT_NAME: "my-project"
}

hooks: {
    onEnter: [{
        command: "echo"
        args: ["Environment configured for development"]
    }]
}

tasks: {
    verify_env: {
        command: "sh"
        args: ["-c", "echo CUENV_TEST=$CUENV_TEST"]
    }
}
```

**Setup:**

```bash
# Approve the configuration (required for security)
cuenv allow

# Shell integration will now auto-load when you cd into this directory
```

## Bun/TypeScript Project

A complete example for a Bun project:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
  name: "my-project"
}

env: {
    NODE_ENV: "development" | "production" | *"development"
    PORT:     3000

    // Database configuration
    DATABASE_URL: "postgresql://localhost:5432/myapp_dev"
    REDIS_URL:    "redis://localhost:6379"

    // API keys (use secrets in production)
    JWT_SECRET: "dev-secret-change-in-production"
}

tasks: {
    // Development tasks
    dev: {
        command: "bun"
        args: ["run", "dev"]
    }

    build: {
        command: "bun"
        args: ["run", "build"]
    }

    // Testing
    test: {
        unit: {
            command: "bun"
            args: ["run", "test:unit"]
        }
        e2e: {
            command: "bun"
            args: ["run", "test:e2e"]
        }
        coverage: {
            command: "bun"
            args: ["run", "test:coverage"]
        }
    }

    // Linting and formatting
    lint: {
        command: "bun"
        args: ["run", "lint"]
    }

    format: {
        command: "bun"
        args: ["run", "format"]
    }

    // Database operations
    db: {
        migrate: {
            command: "bunx"
            args: ["prisma", "migrate", "dev"]
        }
        seed: {
            command: "bunx"
            args: ["prisma", "db", "seed"]
        }
        studio: {
            command: "bunx"
            args: ["prisma", "studio"]
        }
    }

    // CI pipeline
    ci: [
        {command: "bun", args: ["install", "--frozen-lockfile"]},
        {command: "bun", args: ["run", "lint"]},
        {command: "bun", args: ["run", "test"]},
        {command: "bun", args: ["run", "build"]},
    ]
}
```

## Rust Project

A complete example for a Rust project:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
  name: "my-project"
}

env: {
    RUST_LOG:       "info"
    RUST_BACKTRACE: "1"
    DATABASE_URL:   "postgres://localhost/myapp"
}

tasks: {
    // Build tasks
    build: {
        command: "cargo"
        args: ["build"]
    }

    release: {
        command: "cargo"
        args: ["build", "--release"]
    }

    // Testing
    test: {
        command: "cargo"
        args: ["test"]
    }

    // Code quality
    lint: {
        command: "cargo"
        args: ["clippy", "--", "-D", "warnings"]
    }

    format: {
        check: {
            command: "cargo"
            args: ["fmt", "--check"]
        }
        fix: {
            command: "cargo"
            args: ["fmt"]
        }
    }

    // Documentation
    doc: {
        command: "cargo"
        args: ["doc", "--open"]
    }

    // CI pipeline
    ci: [
        {command: "cargo", args: ["fmt", "--check"]},
        {command: "cargo", args: ["clippy", "--", "-D", "warnings"]},
        {command: "cargo", args: ["test"]},
        {command: "cargo", args: ["build", "--release"]},
    ]
}
```

## Environment-Specific Policies

Control which tasks and commands can access sensitive variables:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
  name: "my-project"
}

// Reusable policies
_databasePolicy: schema.#Policy & {
    allowTasks: ["migrate", "db_backup", "db_restore"]
    allowExec: ["psql", "pg_dump"]
}

_deployPolicy: schema.#Policy & {
    allowTasks: ["deploy", "release"]
    allowExec: ["kubectl", "terraform"]
}

env: {
    // Public variables - accessible everywhere
    APP_NAME: "my-app"
    PORT:     8080
    DEBUG:    true

    // Restricted: only database tasks can access
    DB_PASSWORD: {
        value: schema.#Secret
        policies: [_databasePolicy]
    }

    // Restricted: only deploy tasks can access
    DEPLOY_TOKEN: {
        value: schema.#Secret
        policies: [_deployPolicy]
    }
}

tasks: {
    // Can access DB_PASSWORD
    migrate: {
        command: "migrate"
        args: ["up"]
    }

    // Can access DEPLOY_TOKEN
    deploy: {
        command: "kubectl"
        args: ["apply", "-f", "k8s/"]
    }

    // Cannot access restricted variables
    build: {
        command: "bun"
        args: ["run", "build"]
    }
}
```

## Monorepo Configuration

Structure for a monorepo with multiple services:

**Root `env.cue`:**

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
  name: "my-project"
}

// Shared environment for all services
env: {
    ORGANIZATION: "myorg"
    LOG_LEVEL:    "info"
    ENVIRONMENT:  "development"
}

tasks: {
    // Run all services
    dev: {
        all: [
            {command: "cuenv", args: ["task", "dev", "-p", "services/api"]},
            {command: "cuenv", args: ["task", "dev", "-p", "services/web"]},
        ]
    }

    // Build all
    build: {
        all: [
            {command: "cuenv", args: ["task", "build", "-p", "services/api"]},
            {command: "cuenv", args: ["task", "build", "-p", "services/web"]},
        ]
    }

    // Test all
    test: {
        all: [
            {command: "cuenv", args: ["task", "test", "-p", "services/api"]},
            {command: "cuenv", args: ["task", "test", "-p", "services/web"]},
        ]
    }
}
```

Each task shells out to the `cuenv` CLI with `-p` (path) so that every service runs its own `env.cue` configuration.

**`services/api/env.cue`:**

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
  name: "my-project"
}

env: {
    SERVICE_NAME: "api"
    PORT:         8080
    DATABASE_URL: "postgres://localhost/api_dev"
}

tasks: {
    dev: {
        command: "cargo"
        args: ["run"]
    }
    build: {
        command: "cargo"
        args: ["build", "--release"]
    }
    test: {
        command: "cargo"
        args: ["test"]
    }
}
```

## CI/CD Integration

**GitHub Actions example (`.github/workflows/ci.yml`):**

```yaml
name: CI

on: [push, pull_request]

jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Install cuenv
        run: cargo install cuenv-cli

      - name: Run CI pipeline
        run: cuenv task ci
```

**cuenv configuration for CI:**

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
  name: "my-project"
}

env: {
    CI: "true"
    RUST_BACKTRACE: "1"
}

tasks: {
    ci: [
        {command: "cargo", args: ["fmt", "--check"]},
        {command: "cargo", args: ["clippy", "--", "-D", "warnings"]},
        {command: "cargo", args: ["test", "--workspace"]},
        {command: "cargo", args: ["build", "--release"]},
    ]
}
```

## Tools Management

### Basic Tools Configuration

Set up hermetic, reproducible CLI tools:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
    name: "my-project"
}

runtime: schema.#ToolsRuntime & {
    platforms: ["darwin-arm64", "darwin-x86_64", "linux-x86_64"]
    tools: {
        // Simple version strings (uses Homebrew)
        jq: "1.7.1"
        yq: "4.44.6"
        ripgrep: "14.1.1"
    }
}

tasks: {
    process: {
        command: "jq"
        args: [".data", "input.json"]
    }
}
```

**Usage:**

```bash
# Lock tools to create cuenv.lock
cuenv sync lock

# Download all tools
cuenv tools download

# Run task with tools activated
cuenv task process

# Run arbitrary command with tools
cuenv exec -- jq --version
```

### GitHub Release Tools

Fetch tools directly from GitHub Releases:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
    name: "github-tools"
}

runtime: schema.#ToolsRuntime & {
    platforms: ["darwin-arm64", "linux-x86_64"]
    tools: {
        // GitHub CLI
        gh: {
            version: "2.62.0"
            source: schema.#GitHub & {
                repo: "cli/cli"
                tag: "v{version}"
                asset: "gh_{version}_{os}_{arch}.tar.gz"
                path: "gh_{version}_{os}_{arch}/bin/gh"
            }
        }

        // Just command runner with platform overrides
        just: {
            version: "1.40.0"
            overrides: [
                {os: "darwin", source: schema.#GitHub & {
                    repo: "casey/just"
                    tagPrefix: "v"
                    asset: "just-{version}-{arch}-apple-darwin.tar.gz"
                    path: "just"
                }},
                {os: "linux", source: schema.#GitHub & {
                    repo: "casey/just"
                    tagPrefix: "v"
                    asset: "just-{version}-{arch}-unknown-linux-musl.tar.gz"
                    path: "just"
                }},
            ]
        }
    }
}
```

### Rust Development Environment

Complete Rust development setup using contrib modules:

```cue
package cuenv

import (
    "github.com/cuenv/cuenv/schema"
    xRust "github.com/cuenv/cuenv/contrib/rust"
)

schema.#Project & {
    name: "rust-project"
}

runtime: schema.#ToolsRuntime & {
    platforms: ["darwin-arm64", "darwin-x86_64", "linux-x86_64"]
    flakes: nixpkgs: "github:NixOS/nixpkgs/nixos-24.11"
    tools: {
        // Rust toolchain via rustup
        rust: xRust.#Rust & {
            version: "1.83.0"
            source: {
                profile: "default"
                components: ["clippy", "rustfmt", "rust-src", "llvm-tools-preview"]
                targets: ["x86_64-unknown-linux-gnu", "wasm32-unknown-unknown"]
            }
        }

        // LSP server (date-based version)
        "rust-analyzer": xRust.#RustAnalyzer & {version: "2025-12-29"}

        // Testing
        "cargo-nextest": xRust.#CargoNextest & {version: "0.9.116"}

        // Security
        "cargo-deny": xRust.#CargoDeny & {version: "0.18.9"}

        // Coverage
        "cargo-llvm-cov": xRust.#CargoLlvmCov & {version: "0.7.0"}

        // Build caching
        sccache: xRust.#SccacheTool & {version: "0.10.0"}
    }
}

env: {
    RUSTC_WRAPPER: "sccache"
    CARGO_INCREMENTAL: "0"
}

tasks: {
    build: {
        command: "cargo"
        args: ["build"]
    }
    test: {
        command: "cargo-nextest"
        args: ["run"]
    }
    lint: {
        command: "cargo"
        args: ["clippy", "--", "-D", "warnings"]
    }
    coverage: {
        command: "cargo-llvm-cov"
        args: ["--html"]
    }
}
```

### Nix-Based Tools

Use Nix for complex toolchains or tools without GitHub releases:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
    name: "nix-tools"
}

runtime: schema.#ToolsRuntime & {
    platforms: ["darwin-arm64", "linux-x86_64"]
    flakes: {
        nixpkgs: "github:NixOS/nixpkgs/nixos-24.11"
        unstable: "github:NixOS/nixpkgs/nixos-unstable"
    }
    tools: {
        // Python from stable nixpkgs
        python: {
            version: "3.11"
            source: schema.#Nix & {
                flake: "nixpkgs"
                package: "python311"
            }
        }

        // Latest package from unstable
        deno: {
            version: "2.0"
            source: schema.#Nix & {
                flake: "unstable"
                package: "deno"
            }
        }
    }
}
```

### OCI Container Tools

Extract binaries from container images:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
    name: "oci-tools"
}

runtime: schema.#ToolsRuntime & {
    platforms: ["darwin-arm64", "linux-x86_64"]
    tools: {
        kubectl: {
            version: "1.31.0"
            source: schema.#Oci & {
                image: "bitnami/kubectl:{version}"
                path: "/opt/bitnami/kubectl/bin/kubectl"
            }
        }

        helm: {
            version: "3.16.3"
            source: schema.#Oci & {
                image: "alpine/helm:{version}"
                path: "/usr/bin/helm"
            }
        }
    }
}
```

### Mixed Source Configuration

Combine multiple sources with platform overrides:

```cue
package cuenv

import (
    "github.com/cuenv/cuenv/schema"
    xTools "github.com/cuenv/cuenv/contrib/tools"
    xBun "github.com/cuenv/cuenv/contrib/bun"
)

schema.#Project & {
    name: "mixed-tools"
}

runtime: schema.#ToolsRuntime & {
    platforms: ["darwin-arm64", "darwin-x86_64", "linux-x86_64", "linux-arm64"]
    flakes: nixpkgs: "github:NixOS/nixpkgs/nixos-24.11"
    tools: {
        // Contrib modules handle platform complexity
        jq: xTools.#Jq & {version: "1.7.1"}
        yq: xTools.#Yq & {version: "4.44.6"}
        bun: xBun.#Bun & {version: "1.3.5"}

        // Simple Homebrew tool
        ripgrep: "14.1.1"

        // GitHub with fallback to OCI on Linux
        dive: {
            version: "0.12.0"
            source: schema.#Homebrew
            overrides: [
                {os: "linux", source: schema.#Oci & {
                    image: "wagoodman/dive:{version}"
                    path: "/usr/local/bin/dive"
                }}
            ]
        }
    }
}
```

### Tools with Shell Integration

Activate tools automatically when entering a project:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
    name: "interactive-project"
}

runtime: schema.#ToolsRuntime & {
    platforms: ["darwin-arm64", "linux-x86_64"]
    tools: {
        jq: "1.7.1"
        yq: "4.44.6"
    }
}

hooks: {
    onEnter: {
        // Activate tools for interactive shell use
        tools: schema.#ToolsActivate

        // Also load Nix environment if present
        nix: schema.#NixFlake
    }
}
```

**Setup:**

```bash
# Approve the configuration
cuenv allow

# Lock tools
cuenv sync lock

# Now tools are auto-activated when you cd into the directory
cd /path/to/project
jq --version  # Works!
```

## See Also

- [Configuration Guide](/how-to/configure-a-project/) - Detailed configuration reference
- [Tasks](/how-to/run-tasks/) - Task orchestration documentation
- [Environments](/how-to/typed-environments/) - Environment management
- [Secrets](/how-to/secrets/) - Secret management patterns
- [Tools](/how-to/tools/) - Tools management guide
