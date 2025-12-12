---
title: Secrets
description: Secure secret management with cuenv
---

cuenv provides a flexible secret management system that integrates with various secret providers. Secrets are resolved at runtime using exec-based resolvers, keeping sensitive values out of your configuration files.

## How Secrets Work

cuenv uses an exec-based resolver pattern: instead of storing secrets directly in configuration, you define a command that retrieves the secret at runtime. This approach:

- Keeps secrets out of version control
- Integrates with any CLI-based secret provider
- Allows runtime secret rotation without config changes
- Works with existing secret management infrastructure

## Basic Secret Structure

The base `#Secret` type defines the resolver pattern:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
  name: "my-project"
}

env: {
    MY_SECRET: schema.#Secret & {
        resolver: "exec"
        command:  "echo"
        args:     ["my-secret-value"]
    }
}
```

When cuenv loads the environment, it executes the command and uses its output as the secret value.

## Built-in Secret Providers

### 1Password

cuenv includes a built-in 1Password resolver using the `op` CLI:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
  name: "my-project"
}

env: {
    // Reference a 1Password item
    DATABASE_PASSWORD: schema.#OnePasswordRef & {
        ref: "op://vault-name/item-name/password"
    }

    // Another 1Password secret
    API_KEY: schema.#OnePasswordRef & {
        ref: "op://Development/API Keys/production"
    }
}
```

**Prerequisites:**

- Install the [1Password CLI](https://developer.1password.com/docs/cli/)
- Sign in with `op signin` or use a service account

**How it works:**
The `#OnePasswordRef` expands to:

```cue
command: "op"
args: ["read", "op://vault-name/item-name/password"]
```

### Google Cloud Secret Manager

Use GCP Secret Manager for cloud-native secret storage:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
  name: "my-project"
}

env: {
    // GCP secret with default "latest" version
    DB_PASSWORD: schema.#GcpSecret & {
        project: "my-gcp-project"
        secret:  "database-password"
    }

    // Specific version
    API_KEY: schema.#GcpSecret & {
        project: "my-gcp-project"
        secret:  "api-key"
        version: "5"
    }
}
```

**Prerequisites:**

- Install the [Google Cloud CLI](https://cloud.google.com/sdk/docs/install)
- Authenticate with `gcloud auth login`
- Ensure the account has `secretmanager.versions.access` permission

**How it works:**
The `#GcpSecret` expands to:

```cue
command: "gcloud"
args: ["secrets", "versions", "access", "latest", "--secret", "database-password", "--project", "my-gcp-project"]
```

## Custom Secret Providers

Create custom resolvers for any secret provider with a CLI:

### AWS Secrets Manager

```cue
import "github.com/cuenv/cuenv/schema"

#AwsSecret: schema.#Secret & {
    region: string
    name:   string
    command: "aws"
    args: [
        "secretsmanager", "get-secret-value",
        "--region", region,
        "--secret-id", name,
        "--query", "SecretString",
        "--output", "text"
    ]
}

env: {
    DB_PASSWORD: #AwsSecret & {
        region: "us-west-2"
        name:   "prod/database/password"
    }
}
```

### HashiCorp Vault

```cue
import "github.com/cuenv/cuenv/schema"

#VaultSecret: schema.#Secret & {
    path:  string
    field: string
    command: "vault"
    args: ["kv", "get", "-field=\(field)", path]
}

env: {
    API_KEY: #VaultSecret & {
        path:  "secret/myapp/api"
        field: "key"
    }
}
```

### Azure Key Vault

```cue
import "github.com/cuenv/cuenv/schema"

#AzureSecret: schema.#Secret & {
    vault: string
    name:  string
    command: "az"
    args: [
        "keyvault", "secret", "show",
        "--vault-name", vault,
        "--name", name,
        "--query", "value",
        "--output", "tsv"
    ]
}

env: {
    CONNECTION_STRING: #AzureSecret & {
        vault: "my-keyvault"
        name:  "db-connection-string"
    }
}
```

### Doppler

```cue
import "github.com/cuenv/cuenv/schema"

#DopplerSecret: schema.#Secret & {
    project: string
    config:  string
    name:    string
    command: "doppler"
    args: ["secrets", "get", name, "--project", project, "--config", config, "--plain"]
}

env: {
    STRIPE_KEY: #DopplerSecret & {
        project: "backend"
        config:  "production"
        name:    "STRIPE_SECRET_KEY"
    }
}
```

## Best Practices

### 1. Never Commit Secrets

Always use secret references, never hardcode values:

```cue
// WRONG - secret in config
env: {
    API_KEY: "sk-1234567890abcdef"
}

// CORRECT - secret reference
env: {
    API_KEY: schema.#OnePasswordRef & {
        ref: "op://API/Production/key"
    }
}
```

### 2. Use Descriptive References

Make secret paths self-documenting:

```cue
env: {
    // Clear path structure
    STRIPE_SECRET: schema.#OnePasswordRef & {
        ref: "op://Payments/Stripe-Production/secret-key"
    }
}
```

### 3. Create Reusable Definitions

Define organization-specific secret patterns:

```cue
// shared/secrets.cue
package shared

import "github.com/cuenv/cuenv/schema"

#ProdOnePassword: schema.#OnePasswordRef & {
    // Override ref in usage
    ref: string & =~"^op://Production/"
}

#ProdGcpSecret: schema.#GcpSecret & {
    project: "my-company-prod"
}
```

### 4. Test Secret Access

Verify secrets resolve correctly:

```bash
# Check environment with secrets
cuenv env print

# Test specific task's access
cuenv task migrate
```

## Troubleshooting

### Secret Not Resolving

```
error: secret resolution failed
  DB_PASSWORD: command 'op' failed with exit code 1
```

**Fixes:**

1. Verify the CLI tool is installed and in PATH
2. Check authentication status (e.g., `op signin`, `gcloud auth login`)
3. Verify the secret reference/path is correct
4. Test the command manually

### Slow Secret Resolution

If secrets take too long to resolve:

1. Consider caching strategies at the provider level
2. Batch related secrets where possible
3. Use local development secrets for non-production environments

## See Also

- [Configuration Guide](/configuration/) - General configuration patterns
- [Examples](/examples/) - Complete configuration examples
- [Environments](/environments/) - Environment variable management
