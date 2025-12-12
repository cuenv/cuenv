---
title: Quick Start
description: Get up and running with cuenv quickly
---

This quick start guide will get you up and running with cuenv in just a few minutes.

## Prerequisites

Before you begin, make sure you have:

- Rust 1.70+ installed via [rustup](https://rustup.rs/)
- Git for version control
- (Optional) Nix package manager for additional features

## Installation

### Using Nix (Recommended)

```bash
nix profile install github:cuenv/cuenv
```

### From Source

1. Clone the repository:

```bash
git clone https://github.com/cuenv/cuenv.git
cd cuenv
```

2. Install locally:

```bash
cargo install --path crates/cuenv
```

### Using Cargo

Once published to crates.io:

```bash
cargo install cuenv
```

## Your First CUE Environment

1. Create a new project directory:

```bash
mkdir my-cuenv-project
cd my-cuenv-project
```

2. Create a simple `env.cue` file:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
  name: "my-project"
}

// Define your environment variables
env: {
    NODE_ENV: "development" | "production"
    PORT:     3000
}

// Define tasks
tasks: {
    install: {
        description: "Install dependencies"
        command:     "bun"
        args:        ["install"]
    }

    build: {
        description: "Build the application"
        command:     "bun"
        args:        ["run", "build"]
        dependsOn:   ["install"]
    }

    dev: {
        description: "Start development server"
        command:     "bun"
        args:        ["run", "dev"]
        dependsOn:   ["install"]
    }
}
```

3. Run a task:

```bash
cuenv task dev
```

## What's Next?

- Learn about [configuration options](/configuration/)
- Explore [task orchestration](/tasks/) features
- Set up [typed environments](/environments/)
- Integrate with [Nix](/nix-integration/) for package management
- Configure [secret management](/secrets/)

## Common Use Cases

### Monorepo Management

cuenv supports workspaces for managing complex monorepos. Define workspaces in your configuration:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
  name: "my-project"
}

// Shared environment variables
env: {
    LOG_LEVEL: "info"
    RUST_LOG:  "debug"
}

// Workspace configuration (auto-detected from package managers)
workspaces: {
    api: {
        enabled: true
        package_manager: "bun"
    }
    worker: {
        enabled: true
        package_manager: "cargo"
    }
}
```

### Development Workflows

Automate common development tasks:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
  name: "my-project"
}

tasks: {
    test: {
        description: "Run all tests"
        command:     "cargo"
        args:        ["test", "--workspace"]
    }

    lint: {
        description: "Run linting"
        command:     "cargo"
        args:        ["clippy", "--", "-D", "warnings"]
    }

    format: {
        description: "Format code"
        command:     "cargo"
        args:        ["fmt"]
    }

    ci: {
        description: "Run CI pipeline"
        command:     "echo"
        args:        ["CI complete"]
        dependsOn:   ["lint", "test", "format"]
    }
}
```

## Troubleshooting

### Build Issues

If you encounter build issues:

1. Ensure you have the latest Rust version:

```bash
rustup update
```

2. Clean and rebuild:

```bash
cargo clean
cargo build
```

### CUE Evaluation Errors

For CUE-related errors:

- Validate your `.cue` files with the CUE CLI
- Check for syntax errors and type constraints
- Refer to the [CUE Language documentation](https://cuelang.org/docs/)

## Getting Help

- Check the [API Reference](/api-reference/)
- Browse [examples](/examples/) for common patterns
- Open an issue on [GitHub](https://github.com/cuenv/cuenv/issues)
- Join the discussion on [GitHub Discussions](https://github.com/cuenv/cuenv/discussions)
