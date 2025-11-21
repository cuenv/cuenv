---
files: ["**/*.cue", "schema/**/*", "examples/**/*.cue"]
description: Instructions for working with CUE configuration files
---

# CUE Configuration Instructions

## CUE Language Basics

CUE (Configure, Unify, Execute) is a constraint-based configuration language that provides type safety and validation.

### Key Concepts
- **Definitions**: Types start with `#` (e.g., `#Config`)
- **Constraints**: Use `&` to combine constraints
- **Defaults**: Use `*value` syntax for default values
- **Disjunctions**: Use `|` for "or" constraints

## File Structure

### Package Declaration
All CUE files should start with a package declaration:
```cue
package cuenv
```

### Schema Imports
When using schema definitions:
```cue
import "github.com/cuenv/cuenv/schema"

schema.#Cuenv
```

## Environment Configuration

### Environment Variables
Define with type constraints and defaults:
```cue
env: {
    NODE_ENV: "development" | "staging" | "production"
    PORT:     >0 & <65536 & *3000
    DEBUG:    bool | *false
}
```

### Environment-Specific Overrides
```cue
env: {
    // Base configuration
    LOG_LEVEL: "info" | "debug" | "error"
    
    // Production overrides
    environment: production: {
        LOG_LEVEL: "error"
        DEBUG:     false
    }
}
```

## Task Definitions

### Basic Task Structure
```cue
tasks: {
    build: {
        description: "Build the application"
        command:     "npm"
        args:        ["run", "build"]
        inputs:      ["src/**/*"]
        outputs:     ["dist/**/*"]
    }
}
```

### Sequential Tasks (Array)
```cue
tasks: {
    deploy: {
        description: "Deploy application"
        tasks: [
            {command: "docker", args: ["build", "-t", "app", "."]},
            {command: "docker", args: ["push", "app"]},
            {command: "kubectl", args: ["apply", "-f", "k8s/"]}
        ]
    }
}
```

### Parallel Tasks (Object)
```cue
tasks: {
    test: {
        description: "Run all tests in parallel"
        unit:        {command: "npm", args: ["test:unit"]}
        integration: {command: "npm", args: ["test:e2e"]}
        lint:        {command: "npm", args: ["lint"]}
    }
}
```

## Schema Development

### Location
- Core schemas: `schema/` directory
- Example schemas: `examples/` directory
- Generated schemas: `generated-schemas/` directory

### Schema Validation
Test schemas with the schema validator:
```bash
cargo run --bin schema-validator -- validate schema/cuenv.cue
```

### Schema Guidelines
- Use clear, descriptive field names
- Document all fields with comments
- Provide sensible defaults where appropriate
- Use constraints to enforce valid values
- Consider backward compatibility

## Examples

### Location
Place examples in `examples/` directory with descriptive subdirectories:
```
examples/
├── env-basic/         # Basic environment configuration
│   └── env.cue
├── tasks-parallel/    # Parallel task execution
│   └── env.cue
└── secrets/          # Secret management
    └── env.cue
```

### Example Requirements
- Include clear comments explaining the purpose
- Demonstrate best practices
- Keep examples simple and focused
- Ensure examples can be executed with `cargo run`

## Testing CUE Files

### Manual Testing
```bash
# Test basic functionality
cargo run --bin cuenv -- env print --path examples/env-basic --package examples

# Test with specific environment
cargo run --bin cuenv -- env print --path examples/env-basic --package examples --env production

# Test JSON output
cargo run --bin cuenv -- env print --path examples/env-basic --package examples --output-format json
```

### Validation
- Ensure CUE files are syntactically valid
- Verify constraints work as expected
- Test default values
- Validate error messages are helpful

## Common Patterns

### Type-Safe Configuration
```cue
#DatabaseConfig: {
    host:     string
    port:     >0 & <65536
    database: string
    ssl:      bool | *true
}

env: {
    db: #DatabaseConfig & {
        host:     "localhost"
        port:     5432
        database: "myapp"
    }
}
```

### Secret References
```cue
#Secret: {
    resolver: #ExecResolver
    // ... secret definition
}

env: {
    API_KEY: #Secret & {
        resolver: {
            command: "op"
            args: ["read", "op://vault/api/key"]
        }
    }
}
```

## Best Practices

- Keep CUE files focused and modular
- Use imports to share common schemas
- Validate all user inputs with constraints
- Provide helpful error messages via comments
- Document complex constraint logic
- Test configurations with real data
