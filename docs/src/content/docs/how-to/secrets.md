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
redacted before it reaches the terminal. Redaction fails closed: if the secret
registry is ever left in an inconsistent state by an internal panic, cuenv
masks every subsequent output line in full rather than risk printing a secret.

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
infisical` performs an authentication-environment preflight with the same rules
the resolver uses; it does not download files or contact the API.

### Infisical authentication

cuenv reads three kinds of Infisical credentials from its environment:

| Method | Variables | What cuenv does |
| --- | --- | --- |
| Universal Auth | `INFISICAL_CLIENT_ID`, `INFISICAL_CLIENT_SECRET` | Calls `/api/v1/auth/universal-auth/login` |
| OIDC Auth | `INFISICAL_MACHINE_IDENTITY_ID`, `INFISICAL_AUTH_JWT` | Calls `/api/v1/auth/oidc-auth/login` with the identity ID and the JWT |
| Access token | `INFISICAL_TOKEN` | Uses the token as is |

`INFISICAL_ORGANIZATION_SLUG` is optional and is sent with either login.
`INFISICAL_AUTH_JWT` is the variable the Infisical Terraform provider reads by
default too. cuenv never fetches the JWT itself: something outside cuenv (the
`#InfisicalOidc` contributor on GitHub Actions, or your own script on another
CI system) puts it in the environment.

cuenv chooses the method like this:

1. A half-configured login is an error: `INFISICAL_CLIENT_ID` without
   `INFISICAL_CLIENT_SECRET` (or the reverse), or `INFISICAL_MACHINE_IDENTITY_ID`
   without `INFISICAL_AUTH_JWT` (or the reverse). This holds even when another
   method is complete.
2. Complete Universal Auth and complete OIDC Auth together is an error; cuenv
   does not guess which machine identity you meant.
3. Complete Universal Auth or complete OIDC Auth logs in, and `INFISICAL_TOKEN`
   is ignored.
4. Otherwise `INFISICAL_TOKEN` is used.
5. With none of them, resolution fails and names all three options.

The access token a login returns is cached for its lifetime (minus a minute)
and registered for redaction. `INFISICAL_CLIENT_ID`, `INFISICAL_CLIENT_SECRET`,
`INFISICAL_MACHINE_IDENTITY_ID`, `INFISICAL_AUTH_JWT` and `INFISICAL_TOKEN` are
redacted from cuenv output and withheld from infrastructure provider processes
unless the project passes them explicitly.

### Infisical in GitHub Actions with OIDC

Both patterns below authenticate as an Infisical machine identity with
[OIDC Auth](https://infisical.com/docs/documentation/platform/identities/oidc-auth/github)
configured for GitHub (discovery URL and issuer
`https://token.actions.githubusercontent.com`, plus the subject, audiences and
claims you want to allow). No Infisical
credential is stored in GitHub; store the identity ID as the
`INFISICAL_MACHINE_IDENTITY_ID` repository secret (or pass it literally, it is
not a credential on its own). Both contributors are GitHub-only and are not
part of `#DefaultContributors`.

**Pattern A: cuenv resolves the secrets (`#InfisicalOidc`).** Keep your
`#InfisicalSecret` references and add the contributor:

```cue
package cuenv

import (
	"list"
	"github.com/cuenv/cuenv/schema"
	xContributors "github.com/cuenv/cuenv/contrib/contributors"
)

schema.#Project & {
	let _t = tasks

	name: "my-project"

	env: environment: production: {
		DATABASE_URL: schema.#InfisicalSecret & {
			projectId:   "00000000-0000-0000-0000-000000000000"
			environment: "prod"
			secretName:  "DATABASE_URL"
		}
	}

	tasks: deploy: schema.#Task & {command: "./deploy.sh"}

	ci: {
		providers: ["github"]
		contributors: list.Concat([
			xContributors.#DefaultContributors,
			[xContributors.#InfisicalOidc & {
				// Optional: must be an audience the identity allows. Empty (the
				// default) keeps GitHub's default audience, the owner's URL.
				#audience: "https://github.com/my-org"
				// Optional: defaults to the INFISICAL_MACHINE_IDENTITY_ID secret.
				// #identityId: "${{ vars.INFISICAL_IDENTITY_ID }}"
			}],
		])
		pipelines: deploy: {
			environment: "production"
			tasks: [_t.deploy]
		}
	}
}
```

The generated job gets `id-token: write` and a step, after cuenv is installed
and before `cuenv secrets setup infisical` and `cuenv ci`, that requests the
job's ID token, masks it with `::add-mask::` and writes `INFISICAL_AUTH_JWT`
and `INFISICAL_MACHINE_IDENTITY_ID` to `$GITHUB_ENV`. cuenv then logs in with
OIDC Auth and resolves only the references the selected environment declares,
with policies and redaction as for any other secret. Do not also set the
`INFISICAL_CLIENT_ID` and `INFISICAL_CLIENT_SECRET` repository secrets: two
complete login methods is an error. GitHub ID tokens are short-lived (minutes),
and cuenv logs in on its first Infisical reference and reuses the access token
afterwards; set the identity's access token TTL to cover the job, because once
it expires cuenv cannot log in again with the same JWT. See
[`examples/ci-infisical-oidc`](https://github.com/cuenv/cuenv/tree/main/examples/ci-infisical-oidc).

**Pattern B: Infisical's action fetches the secrets
(`#InfisicalSecretsAction`).** The contributor runs
[`Infisical/secrets-action`](https://github.com/Infisical/secrets-action) with
`method: oidc`, which exports every secret in the folder as a job environment
variable (and masks it in the GitHub log). Tasks read the ones they need with
`#EnvPassthrough`:

```cue
package cuenv

import (
	"github.com/cuenv/cuenv/schema"
	xContributors "github.com/cuenv/cuenv/contrib/contributors"
)

schema.#Project & {
	let _t = tasks

	name: "my-project"

	tasks: deploy: schema.#Task & {
		command: "./deploy.sh"
		env: DATABASE_URL: schema.#EnvPassthrough
	}

	ci: {
		providers: ["github"]
		contributors: [
			xContributors.#Cuenv,
			xContributors.#InfisicalSecretsAction & {
				#project: slug: "my-project" // or #project: id: "..."
				#envSlug:    "prod"
				#secretPath: "/deploy"     // default "/"
				// #domain: "https://eu.infisical.com" // default app.infisical.com
				// #tasks: ["deploy"]                  // only these jobs
			},
		]
		pipelines: deploy: tasks: [_t.deploy]
	}
}
```

The trade-off: with Pattern B, cuenv never sees an Infisical reference, so it
cannot scope a secret to the environment or task that declares it, cannot apply
`policies` (they wrap environment values, and `#EnvPassthrough` is task-level
only), and does not register the values for its own redaction (GitHub's log
masking still applies). Every later step of the job, including third-party
actions, can read every exported secret. Pattern B suits projects that already use the
action or need its features (recursive folders, file export); Pattern A keeps
secret access declared and checked in `env.cue`. See
[`examples/ci-infisical-action`](https://github.com/cuenv/cuenv/tree/main/examples/ci-infisical-action).

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
tasks: publish: env: {
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
