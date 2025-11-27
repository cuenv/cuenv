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

:::note
cuenv is currently in alpha development. Installation methods are being finalized.
:::

### From Source (Development)

1. Clone the repository:

```bash
git clone https://github.com/cuenv/cuenv.git
cd cuenv
```

2. Install locally:

```bash
cargo install --path crates/cuenv-cli
```

### Using Cargo (Future)

Once published to crates.io:

```bash
cargo install cuenv-cli
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

// Define your environment variables
env: {
    NODE_ENV: "development" | "production"
    PORT: string | *"3000"
}

// Define tasks
tasks: {
    install: {
        description: "Install dependencies"
        command: "npm"
        args: ["install"]
    }

    build: {
        description: "Build the application"
        command: "npm"
        args: ["run", "build"]
        dependsOn: ["install"]
    }

    dev: {
        description: "Start development server"
        command: "npm"
        args: ["run", "dev"]
        dependsOn: ["install"]
        // Environment overrides can be handled via cuenv flags or different config structure
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

cuenv excels at managing complex monorepos with multiple services:

```cue
package env

// Shared environment
shared: {
    LOG_LEVEL: "info"
    RUST_LOG: "debug"
}

// Service-specific configurations
services: {
    api: {
        environment: shared & {
            SERVICE_NAME: "api"
            PORT: "8080"
        }
    }

    worker: {
        environment: shared & {
            SERVICE_NAME: "worker"
            QUEUE_URL: "redis://localhost"
        }
    }
}
```

### Development Workflows

Automate common development tasks:

```cue
tasks: {
    test: {
        description: "Run all tests"
        command: "cargo"
        args: ["test", "--workspace"]
    }

    lint: {
        description: "Run linting"
        command: "cargo"
        args: ["clippy", "--", "-D", "warnings"]
    }

    format: {
        description: "Format code"
        command: "cargo"
        args: ["fmt"]
    }

    ci: {
        description: "Run CI pipeline"
        dependsOn: ["lint", "test", "format"]
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
