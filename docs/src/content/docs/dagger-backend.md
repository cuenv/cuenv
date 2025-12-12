---
title: Dagger Backend
description: Run cuenv tasks in Dagger containers for reproducible, isolated execution
---

cuenv supports executing tasks inside [Dagger](https://dagger.io) containers, providing hermetic and reproducible builds across any environment.

## Overview

The Dagger backend runs tasks in containerized environments instead of directly on the host. This enables:

- **Reproducibility**: Tasks run in consistent environments regardless of host configuration
- **Isolation**: Tasks cannot interfere with host system or each other
- **Container chaining**: Build multi-stage pipelines where tasks continue from previous container state
- **Cache persistence**: Cache volumes speed up repeated builds

## Prerequisites

1. **Dagger Engine**: Ensure the Dagger engine is installed and running
2. **Feature Flag**: cuenv must be built with the `dagger-backend` feature (enabled by default)

## Configuration

### Global Backend

Set Dagger as the default backend for all tasks in your configuration:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
  name: "my-project"
}

config: {
    backend: {
        type: "dagger"
        options: {
            image: "alpine:latest"
        }
    }
}
```

### Per-Task Configuration

Override the backend or image for specific tasks using the `dagger` block:

```cue
tasks: {
    build: {
        command: "cargo"
        args: ["build", "--release"]
        dagger: {
            image: "rust:1.75-slim"
        }
    }

    test: {
        command: "pytest"
        args: ["-v"]
        dagger: {
            image: "python:3.11-slim"
        }
    }
}
```

## Container Chaining

Use the `from` field to continue from a previous task's container state. This is powerful for multi-stage builds where you install dependencies in one task and use them in subsequent tasks.

```cue
tasks: {
    "setup": {
        command: "sh"
        args: ["-c", "apk add --no-cache curl jq && echo 'Setup complete!'"]
        description: "Install curl and jq into Alpine container"
        dagger: {
            image: "alpine:latest"
        }
    }

    "use-tools": {
        command: "sh"
        args: ["-c", "which curl && which jq && echo '{\"test\": 123}' | jq ."]
        description: "Use tools installed in setup task"
        dependsOn: ["setup"]
        dagger: {
            from: "setup"
        }
    }
}
```

When using `from`:

- The specified task must complete successfully before this task runs
- You don't need to specify `image` when using `from`
- The container state (installed packages, files, etc.) is preserved

## Cache Volumes

Mount cache volumes to persist data across task runs, speeding up builds:

```cue
tasks: {
    install: {
        command: "pip"
        args: ["install", "-r", "requirements.txt"]
        dagger: {
            image: "python:3.11-slim"
            cache: [
                {path: "/root/.cache/pip", name: "pip-cache"},
            ]
        }
    }

    build: {
        command: "cargo"
        args: ["build"]
        dagger: {
            image: "rust:1.75-slim"
            cache: [
                {path: "/root/.cargo/registry", name: "cargo-registry"},
                {path: "/root/.cargo/git", name: "cargo-git"},
            ]
        }
    }
}
```

Cache volumes with the same `name` share data across tasks and runs.

## Secrets

Mount secrets securely into containers as environment variables or files. Secrets are resolved using cuenv's secret resolvers and passed to Dagger without exposing plaintext in logs.

### As Environment Variable

```cue
tasks: {
    deploy: {
        command: "sh"
        args: ["-c", "echo Deploying with token && curl -H \"Authorization: $API_TOKEN\" ..."]
        dagger: {
            image: "alpine:latest"
            secrets: [
                {
                    name: "api-token"
                    envVar: "API_TOKEN"
                    resolver: {
                        resolver: "exec"
                        command: "op"
                        args: ["read", "op://vault/item/field"]
                    }
                },
            ]
        }
    }
}
```

### As Mounted File

```cue
tasks: {
    publish: {
        command: "npm"
        args: ["publish"]
        dagger: {
            image: "node:20-alpine"
            secrets: [
                {
                    name: "npmrc"
                    path: "/root/.npmrc"
                    resolver: {
                        resolver: "exec"
                        command: "op"
                        args: ["read", "op://vault/npm/npmrc"]
                    }
                },
            ]
        }
    }
}
```

## Environment Variables

Environment variables defined in `env` are automatically passed to Dagger containers:

```cue
env: {
    NODE_ENV: "production"
    LOG_LEVEL: "info"
}

tasks: {
    build: {
        command: "npm"
        args: ["run", "build"]
        dagger: {
            image: "node:20-alpine"
        }
    }
}
```

## CLI Override

Force a specific backend from the command line, overriding configuration:

```bash
# Force execution on host (useful for debugging)
cuenv task build --backend host

# Explicitly use Dagger
cuenv task build --backend dagger
```

## Complete Example

A multi-stage build pipeline with caching and secrets:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
  name: "my-project"
}

env: {
    APP_NAME: "myapp"
}

config: {
    backend: {
        type: "dagger"
        options: {
            image: "python:3.11-slim"
        }
    }
}

tasks: {
    "deps": {
        command: "sh"
        args: ["-c", "pip install flask gunicorn && pip freeze > requirements.txt"]
        description: "Install Python dependencies"
        outputs: ["requirements.txt"]
        dagger: {
            cache: [
                {path: "/root/.cache/pip", name: "pip-cache"},
            ]
        }
    }

    "test": {
        command: "pytest"
        args: ["-v", "tests/"]
        description: "Run tests"
        dependsOn: ["deps"]
        dagger: {
            from: "deps"
        }
    }

    "build": {
        command: "sh"
        args: ["-c", "python -m py_compile app.py && echo 'Build successful'"]
        description: "Verify build"
        dependsOn: ["test"]
        dagger: {
            from: "test"
        }
    }

    "deploy": {
        command: "sh"
        args: ["-c", "curl -X POST -H \"Authorization: Bearer $DEPLOY_TOKEN\" https://api.example.com/deploy"]
        description: "Deploy application"
        dependsOn: ["build"]
        dagger: {
            image: "alpine:latest"
            secrets: [
                {
                    name: "deploy-token"
                    envVar: "DEPLOY_TOKEN"
                    resolver: {
                        resolver: "exec"
                        command: "op"
                        args: ["read", "op://vault/deploy/token"]
                    }
                },
            ]
        }
    }
}
```

## See Also

- [Tasks](/tasks/) - Task definition and dependencies
- [Configuration](/configuration/) - General cuenv configuration
- [Secrets](/secrets/) - Secret management and resolvers
