# cuenv

**Two commands. Type-safe environments. Secrets that never leak. Tasks that run in parallel.**

[![License: AGPL v3](https://img.shields.io/badge/License-AGPL%20v3-blue.svg)](https://www.gnu.org/licenses/agpl-3.0)
[![Build Status](https://github.com/cuenv/cuenv/actions/workflows/cuenv-ci.yml/badge.svg)](https://github.com/cuenv/cuenv/actions/workflows/cuenv-ci.yml)
[![Crates.io](https://img.shields.io/crates/v/cuenv)](https://crates.io/crates/cuenv)

> [!WARNING]
> **Rapid iteration in progress.** I'm actively exploring the right APIs and schema to handle everything cuenv needs to do. Expect breaking changes between releases during this period. If you're using cuenv, be prepared for things to break.

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
- **Secrets resolved at runtime** - Pulled from 1Password or custom exec providers, never stored in files or git history
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
- **Opt-in content-aware caching** - Reuse task results when cache policy and inputs allow it
- **Same secret + environment benefits** as `exec`

```cue
import "github.com/cuenv/cuenv/schema"

tasks: {
    // Parallel: unit, integration, and lint run at the same time
    test: schema.#TaskGroup & {
        type: "group"
        unit:        schema.#Task & { command: "npm", args: ["run", "test:unit"] }
        integration: schema.#Task & { command: "npm", args: ["run", "test:e2e"] }
        lint:        schema.#Task & { command: "npm", args: ["run", "lint"] }
    }

    // Sequential: each step waits for the previous
    deploy: schema.#TaskSequence & [
        schema.#Task & { command: "docker", args: ["build", "-t", "myapp", "."] }
        schema.#Task & { command: "docker", args: ["push", "myapp"] }
        schema.#Task & { command: "kubectl", args: ["apply", "-f", "k8s/"] }
    ]

    // Dependencies: build won't start until test completes
    // Note: dependsOn uses CUE references, not strings
    build: schema.#Task & {
        command:   "npm"
        args:      ["run", "build"]
        dependsOn: [test]  // CUE reference for compile-time validation
        inputs:    ["src/**/*", "package.json"]
        outputs:   ["dist/**/*"]
    }
}
```

**Why this matters**: Your test suite runs in parallel. Your CI is faster. Cacheable tasks can reuse results when inputs are unchanged. And every task inherits your validated environment and resolved secrets.

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
    dev:   schema.#Task & { command: "npm", args: ["run", "dev"] }
    build: schema.#Task & { command: "npm", args: ["run", "build"] }
    test:  schema.#Task & { command: "npm", args: ["test"] }
}
EOF

# Run commands with your secure environment
cuenv exec -- npm install
cuenv task dev

# List available tasks
cuenv task

# Generate CI workflows after adding ci.providers and ci.pipelines
cuenv sync ci
```

---

## Use Cases

### Secure Your Secrets

Stop committing `.env` files. Define secrets with any provider—they're resolved only when needed:

```cue
env: {
    // 1Password
    DB_PASSWORD: schema.#OnePasswordRef & { ref: "op://vault/db/password" }

    // AWS Secrets Manager schema. Runtime resolver support is not registered by default.
    API_KEY: schema.#AwsSecret & { secretId: "api-key" }

    // HashiCorp Vault schema. Runtime resolver support is not registered by default.
    STRIPE_KEY: schema.#VaultSecret & { path: "stripe", key: "key" }

    // Or define your own resolver for any CLI
    CUSTOM_SECRET: schema.#ExecSecret & {
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
import "github.com/cuenv/cuenv/schema"

tasks: {
    // These three run at the same time (parallel group)
    lint: schema.#TaskGroup & {
        type: "group"
        check:  schema.#Task & { command: "eslint",   args: ["src/"] }
        types:  schema.#Task & { command: "tsc",      args: ["--noEmit"] }
        format: schema.#Task & { command: "prettier", args: ["--check", "."] }
    }

    // These run one after another (sequential)
    deploy: schema.#TaskSequence & [
        schema.#Task & { command: "npm",     args: ["run", "build"] }
        schema.#Task & { command: "docker",  args: ["build", "-t", "app", "."] }
        schema.#Task & { command: "docker",  args: ["push", "app"] }
        schema.#Task & { command: "kubectl", args: ["rollout", "restart", "deployment/app"] }
    ]

    // This waits for lint to complete first (CUE reference)
    build: schema.#Task & {
        command:   "npm"
        args:      ["run", "build"]
        dependsOn: [lint]  // CUE reference, not string
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

Generate CI workflows from your CUE configuration. Workflow generation requires
explicit provider configuration:

```cue
import "github.com/cuenv/cuenv/schema"

schema.#Project & {
    name: "my-project"

    let _t = tasks

    ci: {
        providers: ["github"]
        pipelines: {
            ci: {
                when: {
                    pullRequest: true
                    branch:      "main"
                }
                tasks: [_t.test]
            }
        }
    }

    tasks: {
        test: schema.#Task & {
            command: "npm"
            args: ["test"]
            inputs: ["package.json", "src/**"]
        }
    }
}
```

Then run:

```bash
# Generate configured GitHub Actions workflows
cuenv sync ci

# Run CI locally
cuenv ci

# Preview what would run
cuenv ci --dry-run
```

GitHub workflow sync is the most complete provider path today. Check the schema
status docs before relying on other provider surfaces.

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

### Code Generation

Generate and sync files from CUE templates—configuration files, boilerplate, and more:

```bash
# Sync all generated files
cuenv sync codegen

# Check if files are in sync (useful in CI)
cuenv sync codegen --check

# Preview changes
cuenv sync codegen --diff --dry-run
```

Define codegen in your CUE configuration to generate TypeScript configs, Dockerfiles, or any templated content.

---

### Multi-Platform VCS Support

cuenv supports GitHub, GitLab, and Bitbucket for CODEOWNERS and CI integration:

```bash
# Sync generated rules for your platform
cuenv sync

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
cuenv env print --output json
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
cuenv sync codegen                         # Sync code from CUE codegen
cuenv sync ci                              # Sync CI workflows
cuenv sync vcs                             # Sync VCS dependencies
cuenv sync --check                         # Check if files are in sync
```

### CI & Release Management

```bash
# CI integration
cuenv ci                                   # Run CI pipeline
cuenv sync ci                              # Generate configured CI workflows
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
```

### Global Options

| Option            | Description                                   |
| ----------------- | --------------------------------------------- |
| `--env, -e`       | Environment to use (dev, staging, production) |
| `-p, --path`      | Directory with CUE files (default: ".")       |
| `--package`       | CUE package name (default: "cuenv")           |
| `--output`        | Command output format where supported         |
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
│   ├── codegen/        # CUE-based code generation
│   ├── ignore/         # Ignore file generation
│   ├── codeowners/     # CODEOWNERS generation
│   ├── github/         # GitHub provider
│   ├── gitlab/         # GitLab provider
│   ├── bitbucket/      # Bitbucket provider
│   └── dagger/         # Dagger task execution backend
├── schema/             # CUE schema definitions
├── examples/          # CUE configuration examples
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
