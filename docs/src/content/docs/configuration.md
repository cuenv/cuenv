---
title: Configuration
description: Comprehensive guide to cuenv configuration
---

Learn how to configure cuenv for your projects using CUE's powerful constraint-based configuration language.

## Configuration Files

cuenv uses CUE files for configuration, following a hierarchical structure that allows for composition and inheritance.

### Default Configuration Files

cuenv looks for configuration in the following order:

1. `cuenv.cue` - Project-specific configuration
2. `env.cue` - Environment definitions
3. `tasks.cue` - Task definitions
4. `.cuenv/` directory - Modular configurations

### Basic Structure

```cue
package cuenv

// Project metadata
project: {
    name:    "my-project"
    version: "1.0.0"
}

// Environment variables
env: {
    NODE_ENV: "development" | "production"
    PORT:     string | *"8080"
    LOG_LEVEL: "info"
}

// Task definitions
tasks: {
    build: {
        description: "Build the project"
        command:     "npm"
        args:        ["run", "build"]
        dependsOn:   ["install"]
    }

    install: {
        description: "Install dependencies"
        command:     "npm"
        args:        ["install"]
    }
}
```

## Environment Configuration

### Basic Environments

Define environment variables with type constraints:

```cue
env: {
    // String variables
    NODE_ENV: "development" | "staging" | "production"
    SERVICE_NAME: string & =~"^[a-zA-Z][a-zA-Z0-9-]*$"

    // Numeric variables (CUE treats environment variables as strings, so conversion might be needed or validation logic adjusted)
    // Typically env vars are strings:
    PORT: string & =~"^[0-9]+$"
    WORKER_COUNT: string | *"4"

    // Boolean variables (as strings)
    DEBUG: "true" | "false" | *"false"
    ENABLE_METRICS: "true" | "false" | *"true"
}
```

### Environment Composition

Compose environments from multiple sources:

```cue
// Base environment
#BaseEnv: {
    LOG_LEVEL: "info" | "debug" | "warn" | "error"
    SERVICE_NAME: string
}

// Development environment
// In cuenv, you typically use a single `env` block with conditional logic or unified types
// But you can define specialized structs:
development: #BaseEnv & {
    LOG_LEVEL: "debug"
    DEBUG: "true"
    DATABASE_URL: "postgresql://localhost/myapp_dev"
}
```

### Conditional Configuration

Use CUE's conditional logic for dynamic configuration:

```cue
package cuenv

// Configuration based on environment
let env = "development"  // or from external source

if env == "development" {
    database: {
        host: "localhost"
        port: 5432
    }
}
```

## Task Configuration

### Basic Tasks

```cue
tasks: {
    test: {
        description: "Run tests"
        command: "cargo"
        args: ["test"]
        env: {
            RUST_LOG: "debug"
        }
    }

    lint: {
        description: "Run linter"
        command: "cargo"
        args: ["clippy", "--", "-D", "warnings"]
    }

    build: {
        description: "Build project"
        command: "cargo"
        args: ["build", "--release"]
        dependsOn: ["lint", "test"]
    }
}
```

### Task Dependencies

Define complex dependency graphs:

```cue
tasks: {
    // Parallel tasks (no dependencies)
    "lint:rust": {
        command: "cargo"
        args: ["clippy"]
    }

    "lint:js": {
        command: "eslint"
        args: ["."]
    }

    // Sequential dependency
    test: {
        command: "cargo"
        args: ["test"]
        dependsOn: ["lint:rust", "lint:js"]  // Waits for both
    }
}
```

### Task Environment Inheritance

```cue
// Shared task environment
#TaskEnv: {
    RUST_LOG: "info"
    PATH: "$PATH:/usr/local/bin"
}

tasks: {
    build: #TaskEnv & {
        command: "cargo build"
        environment: {
            CARGO_TARGET_DIR: "target"
        }
    }

    test: #TaskEnv & {
        command: "cargo test"
        environment: {
            RUST_LOG: "debug"  // Override shared value
        }
    }
}
```

## Schema Definitions

### Custom Schemas

Define reusable configuration schemas:

```cue
// Schema definitions
#DatabaseConfig: {
    host:     string
    port:     int & >0 & <65536
    database: string
    username: string
    password: string  // Should be loaded from secrets
    ssl:      bool | *true
}

#ServiceConfig: {
    name:        string & =~"^[a-zA-Z][a-zA-Z0-9-]*$"
    port:        int & >1024
    replicas:    int & >0 | *3
    environment: [string]: string | number | bool
    database:    #DatabaseConfig
}

// Apply schema to configuration
service: #ServiceConfig & {
    name: "api-server"
    port: 8080
    database: {
        host:     "postgres.local"
        port:     5432
        database: "myapp"
        username: "app_user"
        password: "$DATABASE_PASSWORD"  // From secret
    }
}
```

### Validation Rules

Add custom validation constraints:

```cue
#Config: {
    // Version must follow semantic versioning
    version: string & =~"^[0-9]+\\.[0-9]+\\.[0-9]+$"

    // Port must be available (runtime check)
    port: int & >1024 & <65536

    // Environment must be valid
    env: "development" | "staging" | "production"

    // Features can only be enabled in certain environments
    if env == "production" {
        debug: false
        profiling: false
    }
}
```

## Secret Management

### Secret References

Reference external secrets in configuration:

```cue
environment: {
    // Direct secret reference
    DATABASE_PASSWORD: {
        secret: "database-password"
        key:    "password"
        provider: "1password"  // or "aws-ssm", "gcp-secret-manager"
    }

    // Inline secret with templating
    CONNECTION_STRING: "postgresql://user:${secrets.db.password}@localhost/myapp"
}

// Secret definitions
secrets: {
    db: {
        password: {
            provider: "1password"
            vault:    "Development"
            item:     "Database Credentials"
            field:    "password"
        }
    }
}
```

### Secret Providers

Configure different secret providers:

```cue
secretProviders: {
    "1password": {
        account: "my-team"
        serviceAccountToken: "$OP_SERVICE_ACCOUNT_TOKEN"
    }

    "aws-ssm": {
        region: "us-east-1"
        prefix: "/myapp/"
    }

    "gcp-secret-manager": {
        project: "my-project-123"
    }
}
```

## Advanced Features

### Modular Configuration

Split configuration across multiple files:

**cuenv.cue**

```cue
package config

import (
    "github.com/myorg/myproject/environments"
    "github.com/myorg/myproject/tasks"
)

project: {
    name: "myproject"
    version: "1.0.0"
}

// Include other modules
environment: environments.development
tasks: tasks.common
```

**environments/development.cue**

```cue
package environments

development: {
    NODE_ENV: "development"
    DEBUG: true
    DATABASE_URL: "postgresql://localhost/myapp_dev"
}
```

### Template Functions

Use CUE's built-in functions for dynamic values:

```cue
import "strings"

config: {
    // String manipulation
    serviceName: strings.ToLower("My-Service-Name")  // "my-service-name"

    // Environment-based configuration
    logLevel: {
        if environment.DEBUG {
            "debug"
        }
        if !environment.DEBUG {
            "info"
        }
    }

    // Computed values
    serverAddress: "http://localhost:\(environment.PORT)"
}
```

## Configuration Validation

### Built-in Validation

cuenv automatically validates configuration against schemas when tasks are run.

```bash
# Check environment validity
cuenv env check
```

### Custom Validators

Extend validation with custom rules:

```cue
#Validators: {
    // Port availability check
    portAvailable: {
        field: "port"
        check: "network.portAvailable"
        message: "Port {value} is not available"
    }

    // Database connectivity
    databaseReachable: {
        field: "database.host"
        check: "network.tcpConnect"
        message: "Cannot connect to database at {value}"
    }
}
```

## Best Practices

### Organization

- Keep configuration files small and focused
- Use meaningful names for fields and values
- Group related configuration together
- Document complex constraints with comments

### Security

- Never commit secrets to version control
- Use secret references instead of plain text values
- Validate secret access patterns
- Rotate secrets regularly

### Maintainability

- Use schemas to enforce consistency
- Leverage CUE's composition features
- Keep environment differences minimal
- Test configuration changes

## See Also

- [Typed Environments](/environments/) - Environment management patterns
- [Task Orchestration](/tasks/) - Task definition and execution
- [Secret Management](/secrets/) - Secure secret handling
- [Examples](/examples/) - Common configuration patterns
