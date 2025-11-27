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

schema.#Cuenv

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
cuenv exec -- node server.js
```

## Basic Tasks

Define tasks to automate common workflows:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Cuenv

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

schema.#Cuenv

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

schema.#Cuenv

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

schema.#Cuenv

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

## Node.js/TypeScript Project

A complete example for a Node.js project:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Cuenv

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
        command: "npm"
        args: ["run", "dev"]
    }

    build: {
        command: "npm"
        args: ["run", "build"]
    }

    // Testing
    test: {
        unit: {
            command: "npm"
            args: ["run", "test:unit"]
        }
        e2e: {
            command: "npm"
            args: ["run", "test:e2e"]
        }
        coverage: {
            command: "npm"
            args: ["run", "test:coverage"]
        }
    }

    // Linting and formatting
    lint: {
        command: "npm"
        args: ["run", "lint"]
    }

    format: {
        command: "npm"
        args: ["run", "format"]
    }

    // Database operations
    db: {
        migrate: {
            command: "npx"
            args: ["prisma", "migrate", "dev"]
        }
        seed: {
            command: "npx"
            args: ["prisma", "db", "seed"]
        }
        studio: {
            command: "npx"
            args: ["prisma", "studio"]
        }
    }

    // CI pipeline
    ci: [
        {command: "npm", args: ["ci"]},
        {command: "npm", args: ["run", "lint"]},
        {command: "npm", args: ["run", "test"]},
        {command: "npm", args: ["run", "build"]},
    ]
}
```

## Rust Project

A complete example for a Rust project:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Cuenv

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

schema.#Cuenv

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
        command: "npm"
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

schema.#Cuenv

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
            {command: "cuenv", args: ["task", "dev"], dir: "services/api"},
            {command: "cuenv", args: ["task", "dev"], dir: "services/web"},
        ]
    }

    // Build all
    build: {
        all: [
            {command: "cuenv", args: ["task", "build"], dir: "services/api"},
            {command: "cuenv", args: ["task", "build"], dir: "services/web"},
        ]
    }

    // Test all
    test: {
        all: [
            {command: "cuenv", args: ["task", "test"], dir: "services/api"},
            {command: "cuenv", args: ["task", "test"], dir: "services/web"},
        ]
    }
}
```

**`services/api/env.cue`:**

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Cuenv

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

schema.#Cuenv

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

## See Also

- [Configuration Guide](/configuration/) - Detailed configuration reference
- [Tasks](/tasks/) - Task orchestration documentation
- [Environments](/environments/) - Environment management
- [Secrets](/secrets/) - Secret management patterns
