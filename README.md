# cuenv

**A modern application build toolchain with typed environments and CUE-powered task orchestration**

[![License: AGPL v3](https://img.shields.io/badge/License-AGPL%20v3-blue.svg)](https://www.gnu.org/licenses/agpl-3.0)
[![Build Status](https://github.com/cuenv/cuenv/workflows/CI/badge.svg)](https://github.com/cuenv/cuenv/actions)
[![Crates.io](https://img.shields.io/crates/v/cuenv)](https://crates.io/crates/cuenv)

> **Status**: Alpha - Core evaluation engine complete, CLI and task runner in active development

---

## Overview

cuenv is a next-generation build toolchain that brings type safety and powerful configuration management to application development. Built around CUE's constraint-based type system, cuenv provides a unified solution for environment management, task orchestration, and secure secret handling.

Unlike traditional build tools, cuenv leverages CUE's ability to compose and validate configuration across directory hierarchies, making it particularly well-suited for monorepos and complex project structures. With integrated Nix support, security isolation, and extensible secret management, cuenv provides a complete development environment solution.

**Perfect for:**
- Monorepos requiring consistent environment management
- Teams needing type-safe configuration
- Projects with complex build dependencies
- Security-conscious development workflows

---

## Features

| Feature | Status | Description |
|---------|--------|-------------|
| ✅ **CUE Evaluation Engine** | Complete | Fast, reliable CUE evaluation with Rust performance |
| 🚧 **CLI Interface** | In Development | Task execution and environment management |
| 🚧 **Typed Environments** | In Development | Compose environment constraints from CUE modules |
| 🚧 **Task Orchestration** | In Development | Parallel/sequential execution with smart dependencies |
| 🚧 **Nix Integration** | In Development | Automatic software provisioning via Nix flakes |
| 🚧 **Secret Management** | In Development | Extensible resolvers for 1Password, AWS, GCP, etc. |
| 📋 **Security Isolation** | Planned | Linux namespaces, landlock, eBPF integration |
| 🚧 **Shell Integration** | In Development | Smart hooks for bash, fish, zsh, nushell |
| 🚧 **Dev Tool Integration** | In Development | Seamless Devenv and Flox compatibility |

**Legend:** ✅ Complete • 🚧 In Development • 📋 Planned

---

## Quick Start

### Installation 🚧

```bash
# Install cuenv
cargo install cuenv

# Initialize in your project
cuenv init

# Setup shell integration
cuenv shell setup
```

### Basic Configuration

Create an `env.cue` file in your project root:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Cuenv

// Environment variables with type constraints
env: {
    NODE_ENV: "development" | "staging" | "production"
    PORT:     >0 & <65536 & *3000
    DEBUG:    bool | *false
    
    // Environment-specific overrides
    environment: production: {
        NODE_ENV: "production"
        DEBUG:    false
    }
}

// Task definitions
tasks: {
    build: {
        description: "Build the application"
        command:     "npm"
        args:        ["run", "build"]
        inputs:      ["src/**/*", "package.json"]
        outputs:     ["dist/**/*"]
    }
    
    test: {
        description: "Run tests in parallel"
        unit: {
            command: "npm"
            args:    ["run", "test:unit"]
            inputs:  ["src/**/*.test.js"]
        }
        integration: {
            command: "npm" 
            args:    ["run", "test:integration"]
            inputs:  ["tests/**/*"]
        }
    }
}
```

### Running Tasks 🚧

```bash
# List available tasks
cuenv task

# Run a specific task
cuenv task build

# Run with specific environment
cuenv --env production task build

# Execute with loaded environment
cuenv exec npm start
```

---

## Core Concepts

### Typed Environments 🚧

cuenv uses CUE's constraint system to provide type-safe environment management:

```cue
import (
    "github.com/myorg/postgres/schema"
    "github.com/myorg/redis/schema" 
)

// Compose environment constraints from multiple modules
env: postgres.#Config & redis.#Config & {
    DATABASE_URL: string & =~"^postgresql://"
    REDIS_URL:    string & =~"^redis://"
    API_KEY:      #Secret & {
        resolver: #OnePasswordRef & {
            ref: "op://api-keys/production/key"
        }
    }
}
```

### Task Orchestration 🚧

Control execution flow with CUE's structure:

```cue
tasks: {
    // Array structure = sequential execution
    deploy: {
        description: "Deploy application"
        tasks: [
            {command: "docker", args: ["build", "-t", "myapp", "."]},
            {command: "docker", args: ["push", "myapp"]},
            {command: "kubectl", args: ["apply", "-f", "k8s/"]}
        ]
    }
    
    // Object structure = parallel execution  
    test: {
        description: "Run all tests"
        unit:        {command: "npm", args: ["run", "test:unit"]}
        integration: {command: "npm", args: ["run", "test:e2e"]}
        lint:        {command: "npm", args: ["run", "lint"]}
    }
}
```

### Secret Management 🚧

Extensible secret resolution with multiple providers:

```cue
#OnePasswordRef: #Secret & {
    ref: string
    resolver: #ExecResolver & {
        command: "op"
        args: ["read", ref]
    }
}

#AWSSecretRef: #Secret & {
    region: string
    name:   string
    resolver: #ExecResolver & {
        command: "aws"
        args: ["secretsmanager", "get-secret-value", 
               "--region", region, "--secret-id", name, 
               "--query", "SecretString", "--output", "text"]
    }
}

env: {
    DB_PASSWORD: #OnePasswordRef & {
        ref: "op://vault/database/password"
    }
    API_KEY: #AWSSecretRef & {
        region: "us-west-2"
        name:   "prod-api-key"
    }
}
```

### Shell Integration 🚧

Automatic environment loading with shell hooks:

```cue
hooks: {
    onEnter: [
        // Load Nix environment
        schema.#NixFlake & {preload: true},
        
        // Custom initialization
        {
            command: "echo"
            args: ["Entering cuenv environment..."]
        }
    ]
    
    onExit: [
        {
            command: "echo"
            args: ["Goodbye!"]
        }
    ]
}
```

---

## CLI Reference 🚧

### Commands

```bash
# Task management
cuenv task                           # List all tasks
cuenv task build                     # Run build task
cuenv task test.unit                 # Run specific subtask
cuenv task --parallel build test    # Run multiple tasks

# Environment management  
cuenv env                            # Show current environment
cuenv env --environment production  # Switch to production
cuenv exec -- npm start             # Execute with loaded env

# Project setup
cuenv init                           # Create initial env.cue
cuenv discover                       # Find all CUE packages
cuenv shell setup                    # Configure shell integration

# Development
cuenv cache clear                    # Clear task cache
cuenv --audit task build            # Run in audit mode
cuenv --trace-output task deploy     # Enable tracing
```

### Global Options

| Option | Description |
|--------|-------------|
| `--env, -e` | Environment to use (dev, staging, production) |
| `--cache` | Cache mode (off, read, read-write, write) |  
| `--capability, -c` | Enable specific capabilities |
| `--audit` | Run in audit mode for security analysis |
| `--output-format` | Output format (tui, spinner, simple, tree) |

---

## Advanced Usage

### Monorepo Configuration 🚧

cuenv excels at managing complex monorepo environments:

```
myproject/
├── env.cue              # Root configuration
├── services/
│   ├── api/
│   │   └── env.cue      # API-specific config
│   └── frontend/
│       └── env.cue      # Frontend-specific config
└── shared/
    └── postgres.cue     # Shared schemas
```

**Root `env.cue`:**
```cue
package cuenv

// Global environment
env: {
    PROJECT_NAME: "myproject"
    LOG_LEVEL:    "info" | "debug" | "error" | *"info"
}

// Workspace tasks
tasks: {
    "build-all": {
        description: "Build all services"
        tasks: [
            {command: "cuenv", args: ["task", "build"], dir: "services/api"},
            {command: "cuenv", args: ["task", "build"], dir: "services/frontend"}
        ]
    }
}
```

### CI/CD Integration 🚧

```yaml
# .github/workflows/ci.yml
name: CI
on: [push, pull_request]

jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: cachix/install-nix-action@v22
      - run: curl -fsSL https://cuenv.sh/install | sh
      - run: cuenv task ci.quality  # Run quality checks
      - run: cuenv task ci.test     # Run all tests
      - run: cuenv task ci.build    # Build artifacts
```

### Custom Secret Resolvers 🚧

Extend cuenv with your own secret providers:

```cue
#HashiCorpVaultRef: #Secret & {
    vault_addr: string
    path:       string
    field:      string
    resolver: #ExecResolver & {
        command: "vault"
        args: ["kv", "get", "-address=\(vault_addr)", 
               "-field=\(field)", path]
    }
}

env: {
    SECRET_KEY: #HashiCorpVaultRef & {
        vault_addr: "https://vault.company.com"
        path:       "secret/myapp"
        field:      "api_key"
    }
}
```

---

## Architecture

### CUE Evaluation Engine ✅

The core of cuenv is `cuengine`, a high-performance CUE evaluation engine written in Rust with Go FFI integration:

- **Performance**: Native Rust performance with optimized caching
- **Memory Safety**: Zero-copy string handling and safe FFI boundaries  
- **Reliability**: Comprehensive error handling and recovery
- **Extensibility**: Plugin architecture for custom functions

### Caching Strategy ✅

Intelligent caching system for fast repeated evaluations:

- **Input Tracking**: File modification time and content hashing
- **Dependency Resolution**: Automatic cache invalidation
- **LRU Eviction**: Memory-efficient cache management
- **Configurable TTL**: Flexible expiration policies

### Security Model 📋

Multi-layered security approach (planned):

- **Linux Namespaces**: Process, network, and filesystem isolation
- **Landlock**: Fine-grained filesystem access control  
- **eBPF Integration**: System call monitoring and filtering
- **Capability System**: Principle of least privilege
- **Audit Logging**: Comprehensive security event tracking

---

## Comparison

| Feature | cuenv | Make | Bazel | Taskfile | direnv |
|---------|-------|------|-------|----------|--------|
| Type Safety | ✅ CUE constraints | ❌ | ✅ BUILD files | ❌ | ❌ |
| Monorepo Support | ✅ Native | ⚠️ Basic | ✅ Excellent | ⚠️ Basic | ⚠️ Per-directory |
| Environment Management | ✅ Typed + Secrets | ❌ | ❌ | ❌ | ✅ Basic |
| Task Dependencies | ✅ Smart | ✅ | ✅ Advanced | ✅ Basic | ❌ |
| Parallel Execution | ✅ | ⚠️ -j flag | ✅ | ⚠️ Limited | ❌ |
| Caching | ✅ Content-aware | ❌ | ✅ Advanced | ❌ | ❌ |
| Security Isolation | 📋 Planned | ❌ | ✅ Sandboxing | ❌ | ❌ |
| Shell Integration | 🚧 | ❌ | ❌ | ❌ | ✅ |

---

## Project Status

### Current Phase: Alpha

**Production Ready:**
- ✅ CUE evaluation engine with comprehensive test suite
- ✅ FFI bridge between Rust and Go
- ✅ Caching and retry mechanisms
- ✅ Input validation and error handling

**In Active Development:**
- 🚧 CLI interface and task runner
- 🚧 Shell integration and hooks
- 🚧 Secret management framework
- 🚧 Nix integration layer

**Planned Features:**
- 📋 Security isolation (namespaces, landlock)
- 📋 Web UI and monitoring
- 📋 SaaS offering for teams
- 📋 IDE integrations

### Roadmap

**Q1 2025:** Complete CLI, basic task execution, shell integration  
**Q2 2025:** Secret management, Nix integration, beta release  
**Q3 2025:** Security features, performance optimizations  
**Q4 2025:** SaaS platform, enterprise features

---

## Contributing

We welcome contributions! cuenv is licensed under AGPL-3.0, ensuring it remains open source.

### Development Setup

```bash
# Clone the repository
git clone https://github.com/cuenv/cuenv
cd cuenv

# Install Nix (recommended)
curl -L https://nixos.org/nix/install | sh

# Enter development environment
nix develop
# or with direnv: direnv allow

# Run tests
cargo test

# Check code quality
treefmt && cargo clippy
```

### Architecture Overview

```
cuenv/
├── crates/
│   ├── cuengine/           # Core CUE evaluation engine
│   │   ├── src/
│   │   ├── bridge.go       # Go FFI bridge
│   │   └── tests/
│   ├── cuenv-core/         # Shared types and utilities  
│   └── cuenv-cli/          # CLI interface (upcoming)
├── examples/               # CUE configuration examples
└── docs/                   # Documentation
```

### Testing

- Unit tests: `cargo test`
- Integration tests: `cargo test --test integration_tests`
- Example validation: `cargo test --test examples`
- Coverage: `cargo llvm-cov`

---

## License

Licensed under the [GNU Affero General Public License v3.0](LICENSE).

**Why AGPL?** We believe in keeping cuenv open source while building a sustainable business. The AGPL ensures that any modifications or hosted services using cuenv remain open source, benefiting the entire community.

---

## Links

- **Documentation**: [docs.cuenv.sh](https://docs.cuenv.sh) 🚧
- **Original POC**: [github.com/rawkode/cuenv](https://github.com/rawkode/cuenv)
- **CUE Language**: [cuelang.org](https://cuelang.org)
- **Discussion**: [GitHub Discussions](https://github.com/cuenv/cuenv/discussions)

---

*Built with ❤️ for the open source community*