# cuenv

**Two commands. Type-safe environments. Secrets that never leak. Tasks that run in parallel.**

[![License: AGPL v3](https://img.shields.io/badge/License-AGPL%20v3-blue.svg)](https://www.gnu.org/licenses/agpl-3.0)
[![Build Status](https://github.com/cuenv/cuenv/workflows/ci/badge.svg)](https://github.com/cuenv/cuenv/actions)
[![Crates.io](https://img.shields.io/crates/v/cuenv)](https://crates.io/crates/cuenv)

> **Status**: Beta - Core features complete, new capabilities (CI, release management, codegen) in active development

---

## The Problem

You've been here before:

- **Secrets in `.env` files** that accidentally get committed, logged, or shared
- **"Works on my machine"** because environment variables differ between developers
- **Build scripts that can't run in parallel** so your CI takes forever
- **Copy-paste task definitions** across projects with no validation

cuenv fixes this with two powerful primitives.

---

## Two Primitives, Infinite Possibilities

### `cuenv exec -- <command>`: Run Anything, Securely

```bash
cuenv exec -- npm start
cuenv exec -e production -- ./deploy.sh
cuenv exec -- cargo build --release
```

Every command runs with:

- **Validated environment** - CUE constraints ensure `NODE_ENV` is actually `"development" | "staging" | "production"`, not a typo
- **Secrets resolved at runtime** - Pulled from 1Password, AWS, GCP, Vault—never stored in files, never in git history
- **Environment-specific overrides** - Switch from dev to production with `-e production`

```cue
env: {
    NODE_ENV: "development" | "staging" | "production"
    PORT:     >0 & <65536 & *3000

    // Secrets are resolved at runtime, redacted from logs
    DB_PASSWORD: schema.#OnePasswordRef & {
        ref: "op://vault/database/password"
    }
}
```

**Why this matters**: Your production credentials are never on disk. They're fetched when needed, used, and forgotten. `cuenv env print` shows `[SECRET]` instead of values. Shell exports exclude secrets entirely.

---

### `cuenv task <name>`: Orchestrated, Parallel, Cached

```bash
cuenv task build
cuenv task test
cuenv task -e production deploy
```

Every task runs with:

- **Automatic dependency resolution** - `build` waits for `lint` and `test` if configured
- **Parallel execution** - Independent subtasks run simultaneously
- **Content-aware caching** - Skip tasks when inputs haven't changed
- **Same secret + environment benefits** as `exec`

```cue
tasks: {
    // Parallel: unit, integration, and lint run at the same time
    test: {
        unit:        { command: "npm", args: ["run", "test:unit"] }
        integration: { command: "npm", args: ["run", "test:e2e"] }
        lint:        { command: "npm", args: ["run", "lint"] }
    }

    // Sequential: each step waits for the previous
    deploy: [
        { command: "docker", args: ["build", "-t", "myapp", "."] }
        { command: "docker", args: ["push", "myapp"] }
        { command: "kubectl", args: ["apply", "-f", "k8s/"] }
    ]

    // Dependencies: build won't start until test completes
    build: {
        command:   "npm"
        args:      ["run", "build"]
        dependsOn: ["test"]
        inputs:    ["src/**/*", "package.json"]
        outputs:   ["dist/**/*"]
    }
}
```

**Why this matters**: Your test suite runs in parallel. Your CI is faster. If nothing changed, cached results are used. And every task inherits your validated environment and resolved secrets.

---

## Quick Start

```bash
# Install cuenv
nix profile install github:cuenv/cuenv
# or: cargo install cuenv

# Create configuration
cat > env.cue << 'EOF'
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
  name: "my-project"
}

env: {
    NODE_ENV: "development" | "production"
    API_KEY:  schema.#OnePasswordRef & { ref: "op://dev/api/key" }
}

tasks: {
    dev:   { command: "npm", args: ["run", "dev"] }
    build: { command: "npm", args: ["run", "build"] }
    test:  { command: "npm", args: ["test"] }
}
EOF

# Run commands with your secure environment
cuenv exec -- npm install
cuenv task dev

# List available tasks
cuenv task

# Generate CI workflow (optional)
cuenv ci --generate github
```

---

## Use Cases

### Secure Your Secrets

Stop committing `.env` files. Define secrets with any provider—they're resolved only when needed:

```cue
env: {
    // 1Password
    DB_PASSWORD: schema.#OnePasswordRef & { ref: "op://vault/db/password" }

    // AWS Secrets Manager
    API_KEY: schema.#AWSSecretRef & { region: "us-west-2", name: "api-key" }

    // HashiCorp Vault
    STRIPE_KEY: schema.#VaultRef & { path: "secret/stripe", field: "key" }

    // Or define your own resolver for any CLI
    CUSTOM_SECRET: schema.#ExecResolver & {
        command: "my-secret-tool"
        args:    ["fetch", "my-secret"]
    }
}
```

Secrets are **never written to disk**, **never exported to your shell**, and **redacted from logs**.

---

### Validate Before You Run

Catch configuration errors before they become runtime failures:

```cue
env: {
    // Constrained to valid values only
    NODE_ENV: "development" | "staging" | "production"
    LOG_LEVEL: "debug" | "info" | "warn" | "error"

    // Must match patterns
    DATABASE_URL: string & =~"^postgresql://"
    API_ENDPOINT: string & =~"^https://"

    // Numeric bounds
    PORT: >0 & <65536

    // Defaults that can be overridden
    TIMEOUT: string | *"30s"
}
```

If someone sets `NODE_ENV: "prod"` instead of `"production"`, cuenv tells them immediately.

---

### Run Tasks in Parallel

Object keys run in parallel. Arrays run sequentially. Dependencies are respected automatically:

```cue
tasks: {
    // These three run at the same time
    lint: {
        check:  { command: "eslint",   args: ["src/"] }
        types:  { command: "tsc",      args: ["--noEmit"] }
        format: { command: "prettier", args: ["--check", "."] }
    }

    // These run one after another
    deploy: [
        { command: "npm",     args: ["run", "build"] }
        { command: "docker",  args: ["build", "-t", "app", "."] }
        { command: "docker",  args: ["push", "app"] }
        { command: "kubectl", args: ["rollout", "restart", "deployment/app"] }
    ]

    // This waits for lint to complete first
    build: {
        command:   "npm"
        args:      ["run", "build"]
        dependsOn: ["lint"]
    }
}
```

---

### Share Environments Across a Monorepo

CUE configurations compose naturally. Define once, use everywhere:

```
myproject/
├── env.cue              # Global settings
├── shared/
│   └── database.cue     # Shared DB config
├── services/
│   ├── api/
│   │   └── env.cue      # Inherits global + adds API-specific
│   └── web/
│       └── env.cue      # Inherits global + adds web-specific
```

```cue
// services/api/env.cue
import "github.com/myorg/shared/database"

env: database.#Config & {
    SERVICE_NAME: "api"
    PORT: 8080
}
```

---

### Automatic Shell Integration

When you `cd` into a cuenv project, your shell is configured automatically:

```bash
# Add to .zshrc / .bashrc
eval "$(cuenv shell init zsh)"

# Now just cd into your project
cd ~/projects/myapp
# → Environment loaded automatically
# → Nix packages available (if configured)
# → Ready to work
```

---

### CI Integration

Generate CI workflows from your CUE configuration. cuenv detects affected projects and runs only what's needed:

```bash
# Generate GitHub Actions workflow
cuenv ci --generate github

# Run CI locally
cuenv ci

# Preview what would run
cuenv ci --dry-run
```

cuenv automatically detects your CI provider (GitHub Actions, GitLab CI, etc.) and optimizes pipelines based on which files changed.

---

### Release Management

Manage releases with changesets and automated publishing:

```bash
# Add a changeset describing your changes
cuenv changeset add -s "Added dark mode" -P my-package:minor

# Or generate from conventional commits
cuenv changeset from-commits

# Preview version bumps
cuenv release version --dry-run

# Publish packages in dependency order
cuenv release publish
```

Changesets integrate with conventional commits and automatically calculate semantic version bumps.

---

### Code Generation (Cubes)

Generate and sync files from CUE templates—configuration files, boilerplate, and more:

```bash
# Sync all generated files
cuenv sync cubes

# Check if files are in sync (useful in CI)
cuenv sync cubes --check

# Preview changes
cuenv sync cubes --diff --dry-run
```

Define cubes in your CUE configuration to generate TypeScript configs, Dockerfiles, or any templated content.

---

### Multi-Platform VCS Support

cuenv supports GitHub, GitLab, and Bitbucket for CODEOWNERS and CI integration:

```bash
# Sync CODEOWNERS for your platform
cuenv sync codeowners

# Works with GitHub, GitLab, or Bitbucket
# Platform is auto-detected from your repository
```

---

## CLI Reference

### Core Commands

```bash
# Execute commands with validated environment + resolved secrets
cuenv exec -- npm start                    # or: cuenv x -- npm start
cuenv exec -e production -- ./deploy.sh

# Run named tasks with dependencies, parallelism, caching
cuenv task build                           # or: cuenv t build
cuenv task -e staging test
cuenv task --tui build                     # Rich TUI output
cuenv task -l ci                           # Run all tasks with label "ci"
```

### Environment Management

```bash
# View environment (secrets are redacted)
cuenv env print
cuenv env print --output-format json
cuenv env list                             # List available environments

# Shell integration
cuenv shell init zsh >> ~/.zshrc
cuenv env load                             # Load environment in background
cuenv env status                           # Check hook execution status
```

### Code Generation & Sync

```bash
# Sync generated files from CUE configuration
cuenv sync                                 # Sync all
cuenv sync ignore                          # Generate .gitignore/.dockerignore
cuenv sync codeowners                      # Sync CODEOWNERS file
cuenv sync cubes                           # Sync code from CUE cubes
cuenv sync --check                         # Check if files are in sync
```

### CI & Release Management

```bash
# CI integration
cuenv ci                                   # Run CI pipeline
cuenv ci --generate github                 # Generate GitHub Actions workflow
cuenv ci --dry-run                         # Preview what would run

# Release management
cuenv changeset add                        # Add a changeset entry
cuenv changeset status                     # Show pending changesets
cuenv changeset from-commits               # Generate from conventional commits
cuenv release version                      # Calculate and apply version bumps
cuenv release publish                      # Publish to crates.io
```

### Security & Utilities

```bash
# Security approval for configurations
cuenv allow                                # Approve configuration for hooks
cuenv deny                                 # Revoke approval

# Utilities
cuenv version                              # Show version info
cuenv completions zsh                      # Generate shell completions
cuenv tui                                  # Interactive TUI dashboard
```

### Global Options

| Option            | Description                                   |
| ----------------- | --------------------------------------------- |
| `--env, -e`       | Environment to use (dev, staging, production) |
| `-p, --path`      | Directory with CUE files (default: ".")       |
| `--package`       | CUE package name (default: "cuenv")           |
| `--output-format` | Output format (json, env, simple)             |
| `-L, --level`     | Log level (trace, debug, info, warn, error)   |

---

## How It Compares

| Feature                | cuenv              | Make       | Bazel          | Taskfile   | direnv           |
| ---------------------- | ------------------ | ---------- | -------------- | ---------- | ---------------- |
| Type Safety            | ✅ CUE constraints | ❌         | ✅ BUILD files | ❌         | ❌               |
| Monorepo Support       | ✅ Native          | ⚠️ Basic   | ✅ Excellent   | ⚠️ Basic   | ⚠️ Per-directory |
| Environment Management | ✅ Typed + Secrets | ❌         | ❌             | ❌         | ✅ Basic         |
| Task Dependencies      | ✅ Smart           | ✅         | ✅ Advanced    | ✅ Basic   | ❌               |
| Parallel Execution     | ✅                 | ⚠️ -j flag | ✅             | ⚠️ Limited | ❌               |
| Caching                | ✅ Content-aware   | ❌         | ✅ Advanced    | ❌         | ❌               |
| CI Integration         | ✅ Native          | ❌         | ⚠️ Rules       | ❌         | ❌               |
| Security Isolation     | ✅ Via Dagger      | ❌         | ✅ Sandboxing  | ❌         | ❌               |
| Shell Integration      | ✅                 | ❌         | ❌             | ❌         | ✅               |

---

## Status

| Component             | Status         |
| --------------------- | -------------- |
| CUE Evaluation Engine | ✅ Complete    |
| CLI + Task Runner     | ✅ Complete    |
| Secret Management     | ✅ Complete    |
| Shell Integration     | ✅ Complete    |
| CI Integration        | 🚧 Development |
| Release Management    | 🚧 Development |
| Code Generation       | 🚧 Development |
| Security Isolation    | ✅ Complete    |

---

## Contributing

We welcome contributions! cuenv is licensed under AGPL-3.0, ensuring it remains open source.

### Development Setup

```bash
# Clone the repository
jj git clone https://github.com/cuenv/cuenv
cd cuenv

# Enter development environment
nix develop
# or with direnv: direnv allow

# Project automation (this repo)
cuenv task fmt.check
cuenv task lint
cuenv task test.unit
cuenv task build
```

### Architecture Overview

```
cuenv/
├── crates/
│   ├── cuengine/       # CUE evaluation engine (Go FFI bridge)
│   ├── core/           # Shared types, task execution, caching
│   ├── cuenv/          # CLI and TUI
│   ├── events/         # Event system for UI frontends
│   ├── workspaces/     # Monorepo and package manager detection
│   ├── ci/             # CI pipeline integration
│   ├── release/        # Version management and publishing
│   ├── cubes/          # CUE-based code generation
│   ├── ignore/         # Ignore file generation
│   ├── codeowners/     # CODEOWNERS generation
│   ├── github/         # GitHub provider
│   ├── gitlab/         # GitLab provider
│   ├── bitbucket/      # Bitbucket provider
│   └── dagger/         # Dagger task execution backend
├── schema/             # CUE schema definitions
├── examples/           # CUE configuration examples
└── docs/               # Documentation
```

### Testing

- Unit tests: `cuenv task test.unit`
- BDD tests: `cuenv task test.bdd`
- Coverage: `cuenv task coverage`

---

## License

Licensed under the [GNU Affero General Public License v3.0](LICENSE).

**Why AGPL?** We believe in keeping cuenv open source while building a sustainable business. The AGPL ensures that any modifications or hosted services using cuenv remain open source, benefiting the entire community.

---

## Links

- **Documentation**: [cuenv.dev](https://cuenv.dev) 🚧
- **CUE Language**: [cuelang.org](https://cuelang.org)
- **Discussion**: [GitHub Discussions](https://github.com/cuenv/cuenv/discussions)

---

_Built in 🏴 󠁧󠁢󠁳󠁣󠁴󠁿w for the open source community_
