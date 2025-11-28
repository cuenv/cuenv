---
title: Environments
description: Typed environment management with cuenv
---

cuenv treats environment variables as a typed configuration schema rather than a bag of strings. This approach allows you to validate your application's configuration before it starts, preventing runtime errors caused by missing or invalid environment variables.

## Defining Environments

Environments are defined in the `env` block of your `env.cue` file. You can use CUE's powerful type system to enforce constraints.

### Type Constraints

```cue
package cuenv

env: {
    // Basic types
    HOST: string | *"0.0.0.0"
    PORT: string & =~"^[0-9]+$"  // Regex validation

    // Enumerated values
    NODE_ENV: "development" | "staging" | "production"
    LOG_LEVEL: "trace" | "debug" | "info" | "warn" | "error" | *"info"

    // Complex constraints
    // Must be a valid URL
    DATABASE_URL: string & =~"^postgres://"
}
```

## Environment Composition

You can organize your environment definitions into reusable schemas and compose them. This is particularly useful for monorepos or when sharing configuration across multiple services.

```cue
// shared/schema.cue
package shared

#BaseEnv: {
    LOG_LEVEL: "debug" | "info" | "warn" | "error"
    REGION: string
}

#DatabaseEnv: {
    DB_HOST: string
    DB_PORT: string | *"5432"
}
```

```cue
// env.cue
package cuenv
import "github.com/myorg/myrepo/shared"

env: shared.#BaseEnv & shared.#DatabaseEnv & {
    // Service-specific overrides or additions
    SERVICE_NAME: "auth-service"
    REGION: "us-east-1"
}
```

## Loading Environments

You can load the environment variables into your shell or execute commands with them.

### Using `cuenv exec`

Run a command with the validated environment:

```bash
cuenv exec -- bun start
```

This will:

1. Evaluate the CUE configuration.
2. Validate that all constraints are met (e.g., required variables are present).
3. Execute the command with the environment variables injected.

### Using `cuenv shell`

You can integrate cuenv into your shell to automatically load environment variables when you enter the directory.

See [Shell Integration](/installation/#shell-integration) for setup instructions.

## Validation

Validation happens automatically when you run tasks or use `cuenv exec`. You can also manually check the environment:

```bash
cuenv env check
```

If any constraint fails (e.g., a required string is missing, or a value doesn't match a regex), cuenv will report an error and refuse to proceed.

## Secret Management

For sensitive values, use secret references instead of hardcoding them. The [secret management guide](/secrets/) walks through the built-in resolvers and patterns.
