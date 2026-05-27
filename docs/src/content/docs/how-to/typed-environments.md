---
title: Environments
description: Typed environment management with cuenv
---

cuenv treats environment variables as a typed configuration schema rather than a bag of strings. This approach allows you to validate your application's configuration before it starts, preventing runtime errors caused by missing or invalid environment variables.

## Defining Environments

Environments are defined in the `env` block of your `env.cue` file. You can use CUE's type system to enforce constraints.

### Type Constraints

Every `env` value must resolve to something concrete before cuenv runs — a
literal value, or a constraint paired with a `*default` the value falls back to.
A bare constraint like `PORT: string & =~"^[0-9]+$"` describes what's valid but
has no value, so cuenv rejects it until you give it one.

```cue
package cuenv

env: {
    // A plain value.
    HOST: "0.0.0.0"

    // Constrained, with a default the value falls back to.
    PORT:      string & =~"^[0-9]+$" | *"3000"          // digits only, defaults to 3000
    NODE_ENV:  "development" | "staging" | "production" | *"development"
    LOG_LEVEL: "trace" | "debug" | "info" | "warn" | "error" | *"info"

    // Must start with postgres://, defaults to a local database.
    DATABASE_URL: string & =~"^postgres://" | *"postgres://localhost/app"
}
```

Set a value that breaks a constraint — say `NODE_ENV: "prod"` — and evaluation
fails, pointing at the offending field, before any command runs.

## Environment Composition

You can organize your environment definitions into reusable schemas and compose them. This is particularly useful for monorepos or when sharing configuration across multiple services.

```cue
// shared/schema.cue
package shared

#BaseEnv: {
    LOG_LEVEL: "debug" | "info" | "warn" | "error" | *"info"
    REGION: string
}

#DatabaseEnv: {
    DB_HOST: string | *"localhost"
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

See [Shell Integration](/how-to/install/#shell-integration) for setup instructions.

## Validation

Validation happens automatically when you run tasks or use `cuenv exec`. You can also manually check the environment:

```bash
cuenv env check
```

If any constraint fails (e.g., a required string is missing, or a value doesn't match a regex), cuenv will report an error and refuse to proceed.

## Secret Management

For sensitive values, use secret references instead of hardcoding them. The [secret management guide](/how-to/secrets/) walks through the built-in resolvers, including command-backed secrets, 1Password, and Infisical.
