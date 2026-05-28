---
title: Secrets
description: Secure secret management with cuenv
---

cuenv resolves secrets at runtime. Secret values stay out of `env.cue`,
generated CI workflows, and generated files. Output routed through cuenv is
redacted before it reaches the terminal.

The current user-facing secret types are:

- `schema.#OnePasswordRef` for 1Password references.
- `schema.#InfisicalSecret` for Infisical REST API references.
- `schema.#GcpSecret` for Google Cloud Secret Manager references.
- `schema.#ExecSecret` for custom command-backed providers.

`schema.#AwsSecret` and `schema.#VaultSecret` are present
in the schema, but their default runtime resolvers are not registered yet. Treat
them as schema-visible future work until the schema status page says otherwise.

## 1Password

Use `schema.#OnePasswordRef` when the secret already lives in 1Password:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
    name: "my-project"

    env: {
        DATABASE_PASSWORD: schema.#OnePasswordRef & {
            ref: "op://Development/Postgres/password"
        }

        API_KEY: schema.#OnePasswordRef & {
            ref: "op://Development/API/key"
        }
    }
}
```

For local development, sign in with the 1Password CLI before loading the
environment:

```bash
op signin
cuenv env print
```

For CI, provide the `OP_SERVICE_ACCOUNT_TOKEN` environment variable from the CI
secret store and resolve the cuenv environment with the production environment
selected:

```bash
cuenv exec -e production -- printenv API_KEY
```

## Infisical

Use `schema.#InfisicalSecret` when the secret already lives in Infisical:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
    name: "my-project"

    env: {
        DATABASE_URL: schema.#InfisicalSecret & {
            projectId:   "00000000-0000-0000-0000-000000000000"
            environment: "prod"
            secretName:  "DATABASE_URL"
        }

        API_KEY: schema.#InfisicalSecret & {
            projectId:   "00000000-0000-0000-0000-000000000000"
            environment: "prod"
            secretName:  "API_KEY"
            secretPath:  "/backend"
        }
    }
}
```

For local development, set either Universal Auth credentials:

```bash
export INFISICAL_CLIENT_ID=...
export INFISICAL_CLIENT_SECRET=...
cuenv env print
```

or an existing access token:

```bash
export INFISICAL_TOKEN=...
cuenv env print
```

`INFISICAL_API_URL` can point at another Infisical region or a self-hosted
instance. A secret can also set `apiUrl` directly. `cuenv secrets setup
infisical` performs an authentication-environment preflight; it does not
download files or contact the API.

## Google Cloud Secret Manager

Use `schema.#GcpSecret` when the secret already lives in Google Cloud Secret
Manager:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
    name: "my-project"

    env: {
        DATABASE_URL: schema.#GcpSecret & {
            project: "my-gcp-project"
            secret:  "database-url"
        }

        API_KEY: schema.#GcpSecret & {
            project: "my-gcp-project"
            secret:  "api-key"
            version: "5"
        }
    }
}
```

Authenticate with Application Default Credentials before loading the
environment:

```bash
gcloud auth application-default login
cuenv env print
```

For CI or service accounts, set `GOOGLE_APPLICATION_CREDENTIALS` to the service
account JSON file path and ensure `gcloud` is available. The resolver asks
`gcloud auth application-default print-access-token` for an access token and
then reads Secret Manager over HTTPS. Tests or advanced setups can provide an
already-minted `GOOGLE_OAUTH_ACCESS_TOKEN` instead.

## Custom Command Secrets

Use `schema.#ExecSecret` for any provider that has a CLI:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
    name: "my-project"

    env: {
        DATABASE_PASSWORD: schema.#ExecSecret & {
            command: "op"
            args: ["read", "op://Development/Postgres/password"]
        }
    }
}
```

The command must print the secret value to stdout. cuenv treats the resulting
value as a secret and redacts it from task output routed through the cuenv event
system.

## Task-Local Secrets

Prefer task-local secret scope when only one task needs the value:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
    name: "deployable"

    tasks: {
        deploy: schema.#Task & {
            command: "bash"
            args: ["scripts/deploy.sh"]
            inputs: ["scripts/deploy.sh"]
            env: {
                DEPLOY_TOKEN: schema.#InfisicalSecret & {
                    projectId:   "00000000-0000-0000-0000-000000000000"
                    environment: "prod"
                    secretName:  "DEPLOY_TOKEN"
                }
            }
        }
    }
}
```

Task-local runtime secrets are resolved at execution time. They are not a good
fit for reusable task-result cache entries, so cache eligibility can be skipped
when a task has runtime `env` values.

## CI-Provided Values

When CI already provides a secret as an environment variable, pass it through
instead of resolving it again:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
    name: "ci-project"

    tasks: {
        publish: schema.#Task & {
            command: "cargo"
            args: ["publish"]
            inputs: ["Cargo.toml", "src/**"]
            env: {
                CARGO_REGISTRY_TOKEN: schema.#EnvPassthrough
            }
        }
    }
}
```

If the host variable has a different name, set `name`:

```cue
env: {
    GH_TOKEN: schema.#EnvPassthrough & {name: "GITHUB_TOKEN"}
}
```

## Check Access

Use the same path that will run the task:

```bash
cuenv env print
cuenv exec -e production -- printenv DATABASE_PASSWORD
cuenv task deploy
```

If a secret fails to resolve, test the underlying command or reference directly
first, then rerun the cuenv command with `-L debug`.

## See Also

- [Schema status](/reference/schema/status/) - current secret resolver support
- [Typed environments](/how-to/typed-environments/) - environment value shapes
- [Run tasks](/how-to/run-tasks/) - task execution and cache behavior
