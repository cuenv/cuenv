---
title: Secrets
description: Secure secret management with cuenv
---

A plaintext `.env` checked into a sibling branch, or a database password pasted
straight into a CI YAML pipeline, is a leak waiting to happen. cuenv replaces
both with a single typed reference — `op://Development/Postgres/password` — that
resolves at runtime and is redacted from any output it routes. The reference is
identical everywhere; only the authentication differs: `op signin` or
Application Default Credentials on your laptop, a service-account token in CI.

cuenv resolves secrets at runtime. Secret values stay out of `env.cue`,
generated CI workflows, and generated files. Output routed through cuenv is
redacted before it reaches the terminal.

The current user-facing secret types are:

- `schema.#OnePasswordRef` for 1Password references.
- `schema.#InfisicalSecret` for Infisical REST API references.
- `schema.#AwsSecret` for AWS Secrets Manager references.
- `schema.#GcpSecret` for Google Cloud Secret Manager references.
- `schema.#ExecSecret` for custom command-backed providers.

`schema.#VaultSecret` is present in the schema but has no runtime resolver
registered. It is **schema-only** today — see
[HashiCorp Vault (schema-only)](#hashicorp-vault-schema-only) below and the
[schema status page](/reference/schema/status/) for the authoritative status.

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

1Password has no dedicated example directory; the snippet above is complete on
its own.

## Infisical

Use `schema.#InfisicalSecret` when the secret already lives in Infisical:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
    name: "my-project"

    env: {
        environment: production: {
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

**See also:** the runnable [`examples/ci-infisical`](https://github.com/cuenv/cuenv/tree/main/examples/ci-infisical)
fixture wires these references into a CI pipeline.

## AWS Secrets Manager

Use `schema.#AwsSecret` when the secret already lives in AWS Secrets Manager:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
    name: "my-project"

    env: {
        environment: production: {
            API_TOKEN: schema.#AwsSecret & {
                secretId: "prod/api-token"
            }

            DATABASE_PASSWORD: schema.#AwsSecret & {
                secretId: "prod/database"
                jsonKey:  "password"
            }
        }
    }
}
```

The AWS resolver uses the AWS CLI, so authentication and region selection follow
the standard AWS provider chain: `AWS_*` environment variables, shared config and
credential files, profiles, and instance/task roles. `jsonKey` extracts a field
from JSON `SecretString` values.

**See also:** the runnable [`examples/ci-aws-secrets`](https://github.com/cuenv/cuenv/tree/main/examples/ci-aws-secrets)
fixture shows these references resolved inside a production CI pipeline.

## Google Cloud Secret Manager

Use `schema.#GcpSecret` when the secret already lives in Google Cloud Secret
Manager:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
    name: "my-project"

    env: {
        environment: production: {
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

**See also:** the runnable [`examples/ci-gcp-secret`](https://github.com/cuenv/cuenv/tree/main/examples/ci-gcp-secret)
fixture shows these references resolved inside a production CI pipeline.

## HashiCorp Vault (schema-only)

`schema.#VaultSecret` exists in the schema with `path`, `key`, and `mount`
fields (`mount` defaults to `"secret"`), but **no runtime resolver is
registered**, so it is **schema-only**: it will not resolve today. The
[schema status page](/reference/schema/status/) records `#VaultSecret` as
schema-only until a resolver is added. Do not rely on the following shape
resolving at runtime yet:

```cue
// schema-only: validates, but does NOT resolve at runtime today
DATABASE_PASSWORD: schema.#VaultSecret & {
    path:  "myapp/config"
    key:   "db_password"
    mount: "secret"
}
```

Until a resolver lands, read Vault secrets through `schema.#ExecSecret` running
the `vault` CLI, which is fully supported:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
    name: "my-project"

    env: {
        DATABASE_PASSWORD: schema.#ExecSecret & {
            command: "vault"
            args: ["kv", "get", "-mount=secret", "-field=db_password", "myapp/config"]
        }
    }
}
```

The `vault` CLI authenticates from the standard Vault environment
(`VAULT_ADDR`, `VAULT_TOKEN`, or a configured auth method).

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

## Interpolated Secrets

Sometimes a secret is only part of a larger value — a `DATABASE_URL` that
embeds a password, for example. Instead of storing the whole assembled URL in
your secret store, build it from parts with `schema.#InterpolatedEnv`: an array
where each `schema.#EnvPart` is either a literal string or a `schema.#Secret`.
This is a **Stable** capability defined in `schema/env.cue`.

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
    name: "my-project"

    env: {
        DATABASE_URL: [
            "postgres://app:",
            schema.#OnePasswordRef & {ref: "op://Development/Postgres/password"},
            "@db.internal:5432/app",
        ]
    }
}
```

At runtime cuenv resolves each `#Secret` part, then concatenates all the parts —
literals and resolved secrets — into the final value. Because a resolved secret
participates, the **entire assembled value is treated as a secret and redacted**
from output routed through cuenv. Any resolver type can appear as a part, so you
can mix `#AwsSecret`, `#GcpSecret`, `#ExecSecret`, and literal connection
parameters in the same string.

See [Environments](/how-to/typed-environments/) for how interpolated values fit
alongside plain and constrained environment variables.

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
</content>
</invoke>
