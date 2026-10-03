---
title: Manage infrastructure
description: Declare managed resources in CUE and let cuenv drive Terraform provider plugins directly, with CUE types generated from each provider's own schema and multi-tenant state in Turso.
---

cuenv can manage real infrastructure with the provider ecosystem you already know from Terraform and OpenTofu — without either command line tool. `cuenv infrastructure` (short form: `cuenv i`) launches unmodified `terraform-provider-*` binaries, speaks their gRPC plugin protocol (versions 5 and 6), and stores every managed resource as its own record in a remote [Turso](https://turso.tech) (libSQL) database. Use `--env Dev` or `--env Staging` to select a complete infrastructure configuration and its project environment values.

:::caution[Experimental]
`cuenv infrastructure` is **experimental**, and `cuenv infrastructure --help` says so. Its behavior, command line flags, schema and state layout can still change between releases, and it is not covered by the stability promises of the other commands. `#Infrastructure` is **partial**: create, update, replace, delete, refresh, state upgrades, dependency ordering, fenced state locking and verified registry installs work against real providers. Read [Current limitations](#current-limitations) before pointing it at anything you care about — in particular, Plugin Framework resources that declare a resource identity are not handled yet, and records cannot be moved between state identities yet.

There is no migration path from databases written by earlier development builds of this command; see [Configure the state database](#configure-the-state-database).
:::

## Commands

| Command                                       | Short form              | What it does                                                            |
| --------------------------------------------- | ----------------------- | ----------------------------------------------------------------------- |
| `cuenv infrastructure plan`                   | `cuenv i plan`          | Refresh recorded resources and show what `apply` would change           |
| `cuenv infrastructure apply`                  | `cuenv i apply`         | Lock, plan, confirm, apply                                              |
| `cuenv infrastructure destroy`                | `cuenv i destroy`       | Delete every managed resource the project owns                          |
| `cuenv infrastructure state list`             | `cuenv i state`         | List managed resources recorded for the project (`list` is the default) |
| `cuenv infrastructure state locks`            | `cuenv i state locks`   | List every lock in the state database, for every project                |
| `cuenv infrastructure state remove <address>` | `cuenv i state remove`  | Forget one managed resource without deleting it                         |
| `cuenv infrastructure state recover`          | `cuenv i state recover` | Record changes that could not be recorded earlier                       |
| `cuenv infrastructure state adopt`            | `cuenv i state adopt`   | Make this instance the owner of the project's state                     |
| `cuenv infrastructure unlock`                 | `cuenv i unlock`        | Show who holds the lock, or release it by identifier                    |
| `cuenv infrastructure provider add <source>@<version>` | `cuenv i provider add` | Generate CUE types for a provider release and pin it in `cuenv.lock` |
| `cuenv infrastructure provider remove <source>` | `cuenv i provider remove` | Delete a provider's generated types and its pin                    |
| `cuenv sync infrastructure`                   |                         | Regenerate every pinned provider's types (`--check` fails on drift)     |

`apply` also takes `--allow-separate-state` (see [Moving between identities](#moving-between-identities)), and `unlock` takes `--module` and `--project` to act on the lock of a project that is not the one being evaluated (see [Stale locks](#when-things-go-wrong)).

`i` is the only short form. Everything else — commands, schema definitions, fields — is spelled out in full. Every subcommand honours the global `--json` flag and then prints exactly one JSON document on standard output (events go to standard error); `apply` and `destroy` require `--yes` with `--json`.

## Dev and Staging environments

`--env` selects a case-sensitive name in `infrastructure.environments` and, when present, the matching `env.environment` overlay. The infrastructure entry must contain the complete provider and resource configuration for that environment. The common `infrastructure.state` backend stays outside the selector. cuenv does not merge top-level providers or resources into a named configuration.

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "platform"

infrastructure: {
	state: turso: {
		url: "libsql://platform-acme.turso.io"
		authenticationTokenEnvironmentVariable: "TURSO_AUTH_TOKEN"
	}
	environments: {
		Dev: {
			providers: random: {source: "hashicorp/random", version: "3.9.1"}
			resources: pet: {
				type: "random_pet"
				configuration: {length: 2, separator: "-dev-"}
			}
		}
		Staging: {
			providers: random: {source: "hashicorp/random", version: "3.9.1"}
			resources: pet: {
				type: "random_pet"
				configuration: {length: 2, separator: "-staging-"}
			}
		}
	}
}

env: {
	AWS_REGION: "eu-west-2"
	environment: {
		Dev: {
			AWS_PROFILE: "platform-dev"
			DEPLOY_TOKEN: schema.#OnePasswordRef & {ref: "op://Development/platform/deploy-token"}
		}
		Staging: {
			AWS_PROFILE: "platform-staging"
			DEPLOY_TOKEN: schema.#OnePasswordRef & {ref: "op://Staging/platform/deploy-token"}
		}
	}
}
```

```bash
cuenv i plan --env Dev -p . --package cuenv
cuenv i apply --env Staging -p . --package cuenv --yes
```

Without `--env`, cuenv uses the top-level `infrastructure.providers` and `infrastructure.resources` configuration and the base `env` values. An unknown name fails before connecting to the state backend or starting a provider, and the error lists the declared environments. Names are case-sensitive, and an infrastructure environment name is a letter followed by letters, digits, `_` or `-` (`env.environment` overlays accept any name, but only names of that form can also be infrastructure environments).

Each named environment has its own state identity, lock, owner and recovery records, even though every environment uses the one `infrastructure.state` backend. This keeps `Dev` and `Staging` from planning or deleting one another's resources. Explicit `--env default` is a named identity, separate from the no-flag identity.

All environments share **one state database and one authentication token**: `state` is declared once, above `environments`, and an environment cannot override it. Anyone who holds the token can read and write the records of every environment, so this is a naming boundary, not a security boundary (see [How state is keyed](#how-state-is-keyed)). A per-environment `state` is a follow-up; until then, use separate projects, each with its own database, when environments need separate tokens.

### Moving between identities

State is not moved automatically. The identity without `--env` and each named environment record their resources separately, so a top-level configuration and named environments can coexist (top-level `resources` beside `environments` whose resource addresses do not overlap): each run plans and deletes only what its own identity records. cuenv refuses, with exit code `2`, the situations in which a run would silently act on the wrong identity, and its help names only commands that work from the state recorded at that moment.

- **An environment that would create what the no-flag identity already records.** `apply --env NAME` refuses when it would create a resource whose address (`type.name`) is also recorded without `--env` and not yet recorded for `NAME`. Both identities would claim the same real objects, and the second create can fail or duplicate them. Resources at other addresses are not a conflict, which is what allows the mixed layout above. `plan --env NAME` only warns, because it changes nothing, and `destroy --env NAME` never refuses: it creates nothing. The error names your options: keep those resources in the configuration without `--env`; free their addresses first, either by deleting the objects with `cuenv i destroy` without `--env` (when the top-level `providers` that manage them are still declared) or by forgetting the records with `cuenv i state remove <address>` (the real objects are not touched, so remove or hand them over yourself before creating them again under `NAME`); or, when `NAME` should manage separate objects that happen to share the addresses, run `apply --env NAME --allow-separate-state`. Moving the records themselves (`state move`) is not available yet.
- **A run without `--env` that would create what an environment already records.** The mirror of the previous case: when top-level `resources` and `environments` coexist, `apply` without `--env` refuses when it would create a resource whose address (`type.name`) is recorded for a declared environment and not yet recorded without `--env`. Otherwise both identities would claim the same real object, and a later `destroy --env NAME` would delete the object the top-level configuration manages. The error names the environments and up to three of the addresses. `plan` only warns, and `destroy` never refuses. Its options mirror the ones above: keep those resources in the environment (declare them there, not at the top level); free their addresses first, either by deleting the objects with `cuenv i destroy --env NAME` (everything that environment records is deleted) or by forgetting the records with `cuenv i state remove <address> --env NAME` (the real objects are not touched); or, when the top-level resources should manage separate objects that happen to share the addresses, run `apply --allow-separate-state` without `--env`.
- **No `--env` when only environments are declared.** When `infrastructure.environments` is declared and there are no top-level `resources`, a run without `--env` has nothing to manage. `apply` refuses, and `plan` warns, because the run would report "no changes" or plan to delete whatever is recorded without `--env`. `destroy` without `--env` is the way out of an old layout: it runs when resources are still recorded without `--env` and the top-level `providers` that delete them are still declared, and refuses (with the reason, and `state remove` as the alternative) otherwise. So moving resources from the top level into `environments` is: move the declaration, run `cuenv i destroy` without `--env` (the old objects are deleted), then `cuenv i apply --env NAME`; or keep the objects by running `cuenv i state remove <address>` for each address and adopting them under `NAME` yourself.

Renaming an environment, or removing it from the configuration, strands its state: `--env OLD` can no longer select a configuration, so `plan`, `apply` and `destroy` cannot act on it (their error says that `state remove` forgets the records without touching the real objects, and that restoring the environment's configuration lets `destroy` run). **Destroy an environment's resources before renaming or removing it.** The state commands (`state list`, `state remove`, `state recover`, `state adopt` and `unlock`) still work for an environment that is no longer declared: they need only `infrastructure.state`, print a warning, and use the state recorded under that name. A mistyped name is told apart from a removed environment by the state itself: when the state database holds no records, lock, owner or saved unrecorded change for the name, the command refuses (exit code `2`) and lists the declared environments instead of reporting an empty result. Without `--env`, `state list`, `unlock` and `state recover` also mention the declared environments (resources recorded for each, held locks, unrecorded changes) as notes on standard error and, with `--json`, in the result as `otherEnvironments`; every command cuenv suggests in an error carries the `--env`, `-p` and `--package` of the run that failed.

## Project environment values and secrets

Plan, apply and destroy resolve the selected project environment through cuenv's normal runtime secret resolvers. Existing `#OnePasswordRef`, `#InfisicalSecret`, `#AwsSecret`, `#GcpSecret` and `#ExecSecret` values can be placed in `env` or a named overlay, and the resolved values are passed to provider processes as environment variables. State-only commands (`state list`, `state remove`, `state recover`, `state adopt` and `unlock`) resolve only the configured backend token, so an unavailable provider credential does not prevent listing, recovery or unlocking state.

Use `allowInfrastructure` to restrict a value to the actions that may use it:

```cue
DEPLOY_TOKEN: {
	value: schema.#OnePasswordRef & {ref: "op://Development/platform/deploy-token"}
	policies: [{allowInfrastructure: ["plan", "apply"]}]
}
```

- The names are `plan`, `apply`, `destroy`, `state-list`, `state-remove`, `state-recover`, `state-adopt` and `unlock` (`#InfrastructureAction`). Any other name fails evaluation, and so does a misspelled field such as `allowInfrastucture`.
- A variable that has policies is available to an infrastructure action only when some policy lists that action. `allowTasks` and `allowExec` do not grant it: a variable that was reachable from infrastructure commands before it carried only `allowTasks` is now withheld from them.
- Policy filtering happens before any secret is retrieved. A variable the policy denies is also removed from the environment the provider inherits, so a same-named host variable cannot bypass the policy.
- A secret that cannot be resolved fails the command with an error that names the variable. The error never quotes the secret command's own error output, because a failing command can print the secret or the credentials it was given.

### Secrets in output

Every resolved secret, and the state authentication token, is registered for redaction before any state or provider work starts, as are the values of cuenv's own resolver credentials in its environment (`OP_SERVICE_ACCOUNT_TOKEN`, `OP_CONNECT_TOKEN`, every `OP_SESSION_*`, `INFISICAL_*`, `VAULT_TOKEN`, `CUENV_SECRET_SALT` and `CUENV_SECRET_SALT_PREV`, and the variables a project's `cache.remote.auth` names). cuenv replaces registered values in everything it prints: events on standard output and standard error, the JSON documents and error envelopes (string by string, so a secret that JSON escapes is still found; the names of fields are never rewritten, whatever the secret is), tracing logs, error messages and their help text, and provider log lines and gRPC messages (redacted before control characters are stripped). A provider's JSON log line is parsed and each decoded string redacted, so a secret that Go's JSON encoder writes with `\u0026` for `&` is found too. A multi-line secret is also redacted line by line, and a secret that contains a quote, a backslash or a control character is also redacted as it appears when printed quoted. Where two secrets overlap in a text, the whole overlapping stretch is replaced. Provider log lines that are not warnings or errors are discarded without being redacted. Values shorter than four characters are not redacted. Redaction is a safety net, not a reason to put a secret in `configuration`: resource records still hold sensitive attribute values in plaintext (see [Configure the state database](#configure-the-state-database)).

### Provider environment

Providers are third-party programs, and `providerEnvironment` decides how much of the cuenv process environment they inherit. It is set at the top level and in each environment. An environment does not inherit the top-level value, so when the top level sets `providerEnvironment: "isolated"` every `--env` run must set it on the selected environment too; otherwise the run is refused rather than silently falling back to `inherit`. A top level that says `"inherit"` (or says nothing) needs no such line: an environment without it inherits, which is what the top level does too.

```cue
infrastructure: {
	providerEnvironment: "isolated"
	environments: prod: {
		providerEnvironment: "isolated" // required while the top level is isolated
		// ...
	}
}
```

| Value                 | What the provider receives                                                                                                                                                                                                                                         |
| --------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `"inherit"` (default) | The ambient environment, **minus the credentials of cuenv's own secret resolvers and the state token**, plus the project values the action's policy allows                                                                                                         |
| `"isolated"`          | Only `PATH`, `HOME`, `USER`, `LOGNAME`, `TMPDIR`, the proxy variables (`HTTP_PROXY`, `HTTPS_PROXY`, `NO_PROXY`, `ALL_PROXY` and their lowercase forms) and the TLS variables (`SSL_CERT_FILE`, `SSL_CERT_DIR`), plus the project values the action's policy allows |

In both modes cuenv sets its own plugin handshake variables and a private `TMPDIR`, and the state authentication token is withheld. Variables whose names are not valid unicode are never passed. In `isolated` mode the environment is built from nothing, so only the listed variables and the project values exist; a proxy URL that carries credentials (`http://user:password@proxy:3128`) is passed without them, because the credentials were not given to the provider on purpose (cuenv logs a warning naming the variable). To use an authenticated proxy, pass the full URL explicitly in the project's `env` under the same name.

The withheld resolver variables are `OP_SERVICE_ACCOUNT_TOKEN`, `OP_CONNECT_TOKEN`, every `OP_SESSION_*` (the 1Password CLI's session tokens), `OP_CONNECT_HOST` (not a secret, so it is withheld but not redacted), `INFISICAL_TOKEN`, `INFISICAL_CLIENT_ID`, `INFISICAL_CLIENT_SECRET`, `VAULT_TOKEN`, `CUENV_SECRET_SALT`, `CUENV_SECRET_SALT_PREV`, and the variables the project's `cache.remote.auth` names (`bearerTokenEnv` and `header.valueEnv`). To give a provider one of them, pass it explicitly in the project's `env` under the same name (subject to its policy).

:::caution[Hygiene, not a sandbox]
Withholding variables and `isolated` mode keep secrets out of a provider's environment by accident; they do not stop a provider that wants them.

- A provider runs as you. In `isolated` mode `HOME` is still passed, so a provider can read `~/.aws`, `~/.vault-token`, `~/.config/op`, `~/.terraformrc` and `credentials.tfrc.json`, and anything else your user can read.
- Every provider receives every project variable the action's policy allows. There is no per-provider scoping: a secret you pass for one provider reaches all the others in the same run. Policies (`allowInfrastructure`) choose which actions may use a variable, not which provider.
- On Linux, cuenv marks its own process non-dumpable (`prctl(PR_SET_DUMPABLE, 0)`) as soon as it starts, so a provider cannot read cuenv's `/proc/<pid>/environ` or memory, which would otherwise hold every ambient variable that was withheld from it. This does not exist on macOS, where a process of the same user may be able to read another's environment. A side effect is that a same-user debugger cannot attach to cuenv and cuenv writes no core dump.
- Credentials that providers legitimately read are not withheld in `inherit` mode: the `AWS_*` variables, `GOOGLE_APPLICATION_CREDENTIALS` and similar cloud credentials stay, because the AWS and Google providers need them. A continuous integration runner's own tokens (an OIDC request token, `GITHUB_TOKEN`) and anything else in the environment are visible to every provider too.

Run providers you do not fully trust under operating-system isolation instead: a container, a virtual machine or a separate user account whose home directory holds no credentials.
:::

## How state is keyed

State is multi-tenant by construction. Every record is keyed by:

| Key           | Source                                                       | Example                    |
| ------------- | ------------------------------------------------------------ | -------------------------- |
| Tenant        | `module:` in `cue.mod/module.cue`, without the `@vN` suffix  | `github.com/acme/platform` |
| Discriminator | the project's `name`                                         | `web`                      |
| Environment   | the global `--env` selector; empty without `--env`           | `Dev`                      |
| Address       | resource `type` and its name in the selected `resources` map | `random_pet.server`        |

cuenv refuses to run without a CUE module path, and `plan`, `apply` and `destroy` refuse to run when another instance anywhere in the module — any directory, any CUE package — has the same `name` and an `infrastructure` block; otherwise the two would share state and each would plan to delete the other's resources. CUE instances in the same package inherit fields from their parent directories, so a child directory shares its parent's `name` (it cannot set its own) and, if it inherits the `infrastructure` block too, is a conflict; put the child in a different CUE package or move its files. The check evaluates every instance in the module and fails closed: if any instance cannot be evaluated, the command stops and names it, and a target the module walk cannot see (a directory starting with `_` or `.`, a `testdata` directory, a nested module) is refused with the reason.

The state database also records which instance (`<directory>:<package>`) owns each project and environment, claimed by the first `apply` or `destroy`. Any other instance using the same module path, project name and environment — another checkout, a nested module, a copied directory — is refused until you move ownership explicitly with `cuenv i state adopt`, run from the instance that should own it. `plan`, `apply` and `destroy` check the owner as soon as the state store is connected, before any of the project's secrets is resolved (an `#ExecSecret` runs a command, and nothing should run for a refused run); only the state backend's token is resolved first. `state remove` and `state recover` with pending changes check the recorded owner while holding the lock; even `state recover --force` requires ownership. They do not claim previously unowned state. `state list`, `unlock` and empty recovery skip ownership checks. State operations other than `state adopt`, and `unlock`, skip the module-wide name check so a broken sibling does not prevent access. Moving a project to a different module or renaming it starts from empty state.

:::caution[Tenancy is a naming boundary, not a security boundary]
The module path is declared by the project itself. Anyone holding a database token can read or write every tenant in that database, including every named environment of a project. For isolation between teams or customers, give each tenant its own Turso database and token.
:::

## Typed configuration

`configuration` is untyped by default: CUE accepts anything, and mistakes surface at plan time from the provider, without file positions. Generate types from the provider's own schema to have CUE reject them first.

### Add a provider

```bash
cuenv infrastructure provider add hashicorp/random@3.9.1
```

This installs the release exactly as `plan` would, reads its schema over gRPC, writes CUE packages into your CUE module's `cue.mod/gen`, and pins the release in the module's `cuenv.lock`:

```text
cue.mod/gen/registry.terraform.io/hashicorp/random/
├── provider.cue                        package random_provider: #Provider, #Configuration
└── resources/
    ├── random_integer/resource.cue     package random_integer: #Resource, #Configuration
    ├── random_password/resource.cue    package random_password
    ├── random_pet/resource.cue         package random_pet
    └── ...
```

Nothing is published or fetched from a CUE registry, and `cue.mod/module.cue` is untouched: CUE resolves imports from `cue.mod/gen` on its own. Your CUE files are not changed either; the command prints the imports to add.

Commit `cuenv.lock`, not the generated types. Like managed [codegen](/how-to/codegen/) files, the provider directories are listed in a `cuenv infrastructure` section of the module's `.gitignore`, and `cuenv sync` recreates them from `cuenv.lock`. Run it after cloning and after pulling a lock change, as you would for codegen: until it has run, every command that evaluates the project — `cuenv task`, `cuenv env`, the shell hook, `cue` and the CUE language server — fails with `cannot find package "registry.terraform.io/..."`. The first `cuenv sync` downloads each pinned provider (or takes it from the plugin cache) to read its schema.

`<source>` is `namespace/type` or `hostname/namespace/type` (a hostname with a port cannot be an import path and is refused); `<version>` is an exact version. `--path` selects a directory inside the CUE module when you are not in it.

### Use the types

```cue
package cuenv

import (
	"github.com/cuenv/cuenv/schema"

	"registry.terraform.io/hashicorp/random:random_provider"
	"registry.terraform.io/hashicorp/random/resources/random_pet"
	"registry.terraform.io/hashicorp/random/resources/random_password"
)

schema.#Project

name: "web"

infrastructure: {
	state: turso: url: "libsql://platform-acme.turso.io"

	providers: random: random_provider.#Provider

	resources: {
		pet: random_pet.#Resource & {
			configuration: {length: 3, separator: "-"}
		}
		database_password: random_password.#Resource & {
			dependsOn: ["pet"]
			configuration: {length: 24, special: false}
		}
	}
}
```

- **`#Provider`** sets `source`, `version` and `schemaDigest` to the release the types describe, and types `configuration` with the provider block. It is closed: writing `version: "3.9.0"` next to it, or adding a `path`, is a CUE conflict.
- **`#Resource`** sets `type` and types `configuration`. It stays open for `provider` and `dependsOn`.
- **`#Configuration`** in each package is the closed definition behind them, if you want to type a value on its own.

Evaluation now fails — with the field and its file position — on a misspelled argument (`field not allowed`), a wrong type (`conflicting values "three" and number`), a missing required argument (`field is required but not present`) and a computed-only attribute set by hand. Provider-side rules (ranges, mutually exclusive arguments) are not in the schema, so the provider still checks them at plan time.

Field names are Terraform's attribute names (`min_lower`, `override_special`). Each field carries the provider's description as a comment, and comments say when the provider chooses a value when the field is not set, when a value is sensitive, and when an argument is deprecated. Optional arguments are `name?: T` without `| null`: Terraform treats an explicit `null` like an absent argument, so leave it out instead. Nested blocks become structs, lists of structs or maps of structs, with `list.MinItems`/`list.MaxItems` where the provider sets bounds.

:::note[Names that cannot be shadowed]
CUE resolves an identifier to the nearest enclosing field of that name before an import. That is why the provider package is `random_provider` (imported with the `:random_provider` qualifier) rather than `random`: inside `providers: random: ...` the field `random` would hide it. The same applies to resource packages: do not give a resource the key `random_pet` if you also refer to the `random_pet` package inside `resources`, or import it under another name (`randomPet "registry.terraform.io/hashicorp/random/resources/random_pet"`). Attributes named after CUE's types (`number`, `string`, `bool`, `list`) are written with quoted labels (`"number"?: bool`) so they cannot shadow the types either; set them as usual.
:::

### Keep types and providers in step

The generated files, the lock and the provider binary are tied together by the schema digest, a SHA-256 of everything the types encode:

- `cuenv i plan`, `apply` and `destroy` compute the digest of the provider they launch and refuse it when it differs from `schemaDigest` (set by `#Provider`) or from the digest `cuenv.lock` pins.
- When `cuenv.lock` pins a provider, its `version` must match the configuration, and installation and cache reuse require the archive SHA-256 the lock records for the current platform. `provider add` records every platform the release is published for, so a lock made on Linux works on macOS.
- `cuenv sync infrastructure` regenerates every pinned provider's types and the `.gitignore` section from `cuenv.lock`; `--check` fails when files are missing or differ, and `--dry-run` reports what would change. It reads only `cuenv.lock`, never your CUE, so it can restore packages your project imports but that are missing. Plain `cuenv sync` includes it; in CI, run `cuenv sync` before anything that evaluates the project.
- To upgrade, run `provider add` with the new version: the types are regenerated and the pin moves. Fix whatever CUE now reports.
- `cuenv infrastructure provider remove hashicorp/random` deletes the generated directory, its `.gitignore` entry and the pin.

cuenv only replaces or removes a `cue.mod/gen` directory whose `provider.cue` starts with its generated-code header, and refuses otherwise. A `cuenv.lock` that pins infrastructure providers is lockfile format version 5; cuenv versions that do not know the section refuse such a lockfile rather than drop it. Lockfiles without the section stay at version 4.

What is not generated yet: data sources, computed attributes for references between resources (see [Current limitations](#current-limitations)), provider functions and ephemeral resources; write-only arguments are left out because cuenv does not send them. Local `path` providers cannot be added yet, so they stay untyped. A module holds one version of each provider source. Very large providers generate one package per resource type (`hashicorp/aws` has about 1700), so evaluation only pays for the resources you import, but the generated directory is large and `cuenv sync` writes all of it.


## A minimal example without typed configuration

[`examples/infrastructure-random`](https://github.com/cuenv/cuenv/tree/main/examples/infrastructure-random) uses untyped `configuration`; the provider still validates every argument at plan time. The end-to-end tests use it because they swap in a local provider `path`.

```cue
infrastructure: {
	state: turso: url: "http://127.0.0.1:8080"

	providers: random: {
		source:  "hashicorp/random"
		version: "3.9.1"
	}

	resources: {
		pet: {
			type: "random_pet"
			configuration: {
				length:    2
				separator: "-"
			}
		}
		port: {
			type:      "random_integer"
			dependsOn: ["pet"]
			configuration: {
				min: 8000
				max: 8999
				keepers: pet: "rotate-with-pet"
			}
		}
	}
}
```

Run it against a local libSQL server, or point `url` at a Turso database:

```bash
sqld --http-listen-addr 127.0.0.1:8080          # local libSQL server
cuenv i plan    -p examples/infrastructure-random --package examples
cuenv i apply   -p examples/infrastructure-random --package examples
cuenv i state   -p examples/infrastructure-random --package examples
cuenv i destroy -p examples/infrastructure-random --package examples
```

### Test Create Stack, Edit Stack and Destroy Stack

The ignored infrastructure suites run against real provider binaries and a Turso/libSQL server, and they run in continuous integration as the `cuenv-infrastructure-e2e` flake check (Linux), which starts `sqld` instances, builds the fake provider and uses the nixpkgs `random`, `local` and `tfe` providers. To run the CLI lifecycle tests yourself, start `sqld` in a separate terminal with a disposable database, then run from the repository development shell:

```bash
CUENV_INFRASTRUCTURE_TEST_RANDOM_PROVIDER=/absolute/path/terraform-provider-random \
CUENV_INFRASTRUCTURE_TEST_TURSO_URL=http://127.0.0.1:8080 \
cuenv exec -- cargo test -p cuenv --test infrastructure_lifecycle -- --ignored
```

The migration fence test and the layout test each require their own empty disposable server, because both create and drop cuenv's tables and the test runner runs them in parallel; each refuses a database that already has infrastructure tables:

```bash
CUENV_INFRASTRUCTURE_TEST_TURSO_MIGRATION_URL=http://127.0.0.1:8081 \
CUENV_INFRASTRUCTURE_TEST_TURSO_LAYOUT_URL=http://127.0.0.1:8082 \
cuenv exec -- cargo test -p cuenv-infrastructure --lib -- --ignored \
  a_migration_refuses_while_any_lock_is_held \
  layouts_and_waiting_migrations_against_a_server
```

Supply `TURSO_AUTH_TOKEN` if the test server requires authentication. The lifecycle tests copy `examples/infrastructure-random/env.cue` to a temporary CUE module, use a unique project name and the supplied provider path, and keep caches and recovery files in temporary user directories. The first test verifies that planning writes no state, applying creates the pet, password and port, and a second apply has no work. It then edits the pet length from two to three in the CUE file, plans and applies its replacement, checks the new identifier and length in durable state, destroys all three resources and verifies that state and locks are empty. A second destroy must also have no work. The second test runs a named `--env dev` stack whose provider receives an `#ExecSecret` through `allowInfrastructure`, and checks that the secret reaches the provider, is not stored in state and never appears in the command's output. Default test runs skip these tests and need neither network access nor provider downloads.

## Configure the state database

```cue
infrastructure: state: turso: {
	url: "libsql://platform-acme.turso.io"
	authenticationTokenEnvironmentVariable: "TURSO_AUTH_TOKEN" // default
}
```

- `url` accepts `libsql://`, `https://` and `wss://`. Plain `http://` and `ws://` are accepted only for loopback addresses (a local `sqld`), so the token never crosses a network in cleartext. URLs must not carry credentials, queries or fragments.
- The authentication token is read from the resolved project environment when that variable is declared there, otherwise from the caller's environment. It is never written to CUE or state, never shown in errors or logs, and **withheld from provider processes**. Create one with `turso db tokens create <database>`.
- cuenv creates and migrates its tables right before any command takes the lock. There is one table family, `cuenv_infrastructure_resources`, `cuenv_infrastructure_locks` and `cuenv_infrastructure_owners`, plus `cuenv_infrastructure_pending_migration` (see below) and `cuenv_infrastructure_migrations`, which records the schema version. Every row is keyed by module path, project name and environment; the environment is empty for a run without `--env`, and a named environment is a separate identity with no fallback to or from the no-flag one. Resource rows carry a generation (a UUID that is replaced when an address is deleted and created again) and a serial. Other commands (`plan`, `state list`, `unlock`, `state recover` with nothing to recover) never create or migrate tables, so they work with a read-only token and report empty state for a fresh database. A database whose schema is newer than the running cuenv is refused rather than misread.
- The schema is at version 1, and no released cuenv has ever written these tables. Later versions will migrate in place, and a migration refuses to run while any run holds a lock (it is checked inside the migration's transaction), so a run that is applying changes never has its writes land in a half-migrated shape. A migration that finds locks held waits up to 30 seconds for them (announcing itself in `cuenv_infrastructure_pending_migration`, so that no new lock is taken meanwhile and a steady stream of short runs cannot starve it; the announcement expires by itself after a minute if the migrating process dies), then fails with every blocking lock listed: module, project, environment, lock identifier, holder and age. That failure exits `4` with the JSON code `infrastructure_locked` and `blockingLocks`, and its help names the command that releases each lock. One stale lock left by a dead run, of any project sharing the database, can therefore be found with `cuenv i state locks` and released with `cuenv i unlock <lock identifier> --module <module path> --project <project> [--env <name>]`, without evaluating that project.
- **Databases written by earlier development builds of this command are not adopted, and never mistaken for newer ones.** Those builds used different table layouts and recorded schema versions 1 to 5 in `cuenv_infrastructure_schema`; this cuenv records its versions in `cuenv_infrastructure_migrations`, so the two cannot collide. A database that holds the tables of a development build (`cuenv_infrastructure_schema`, any `cuenv_infrastructure_environment_*` table, or the misspelled `cuenv_infrastructurestructure_*` family) is refused by every command, reads included, as "written by an unreleased development build of cuenv" with the tables named; a database that holds tables with cuenv's own names but no migration record is refused as a schema conflict. Neither is a connection problem. Drop every table whose name starts with `cuenv_infrastructure` before using the command, which discards the state recorded there; for a database that manages real resources, record them first and use a fresh database. Recovery files written by those builds (format versions 1 to 4 without the `kind` marker current files carry) are refused the same way, naming the file.
- Transient failures (timeouts, 5xx, 429) are retried with backoff. Redirects are never followed, and plaintext loopback URLs bypass `HTTP_PROXY`.

:::caution
Like Terraform state, resource records contain every attribute the provider returns, including values marked sensitive (for example `random_password.result`). Treat the database as secret material and scope its tokens accordingly.
:::

## Configure providers

```cue
infrastructure: providers: {
	// Installed from registry.terraform.io into the plugin cache.
	random: {
		source:  "hashicorp/random"
		version: "3.9.1"
	}
	// A local binary, relative to the project directory.
	cloudflare: {
		source: "cloudflare/cloudflare"
		path:   "bin/terraform-provider-cloudflare_v5.26.0"
	}
}
```

- `source` is `namespace/type` or `hostname/namespace/type`.
- Every resource's provider — its explicit `provider`, or the prefix of its `type` — must be declared here. `dependsOn` entries must name declared resources. Planning checks all provider references, dependency edges and cycles up front, before reading state or starting any provider.
- Set exactly one of `version` (an exact semantic version; constraints such as `~> 3.7` are not supported) or `path`. The engine validates every provider source and install choice up front, including unused declarations, before reading state or starting any provider.
- Downloads must be HTTPS, are verified against the registry's SHA-256 checksum (and, for a provider pinned in `cuenv.lock`, against the pinned checksum — see [Typed configuration](#keep-types-and-providers-in-step)), and are cached using Terraform's layout in `$TF_PLUGIN_CACHE_DIR` when set, otherwise in your platform's cache directory under `cuenv/infrastructure/providers` (`~/.cache` on Linux, `~/Library/Caches` on macOS). cuenv records a manifest with the binary's SHA-256 and re-verifies it on every use; a cache populated by Terraform is reinstalled once. That manifest only detects accidental corruption: anyone who can write to the cache can replace a binary and its manifest together, so never share a writable plugin cache between trust boundaries. The registry's GPG signature is not verified yet.
- With generated types, `providers: random: random_provider.#Provider` sets `source`, `version` and `schemaDigest` for you (see [Typed configuration](#typed-configuration)).
- `configuration` is the provider block. Keep credentials out of it: providers read their usual environment variables (`CLOUDFLARE_API_TOKEN`, `AWS_PROFILE`, …) from the caller's environment as limited by `providerEnvironment` (see [Provider environment](#provider-environment)), with authorized cuenv project values overlaid. Secret-typed resource arguments are not supported yet.

## Declare resources

```cue
infrastructure: resources: {
	zone_settings: {
		type: "cloudflare_zone_setting"
		configuration: {
			zone_id:    "..."
			setting_id: "always_use_https"
			value:      "on"
		}
	}
	web_dns: {
		type:      "cloudflare_dns_record"
		provider:  "cloudflare"     // defaults to the type prefix
		dependsOn: ["zone_settings"] // must be declared; apply after, destroy before
		configuration: {
			zone_id: "..."
			name:    "www"
			type:    "CNAME"
			content: "example.pages.dev"
			ttl:     1
		}
	}
}
```

With generated types, write `web_dns: cloudflare_dns_record.#Resource & {dependsOn: ["zone_settings"], configuration: {...}}` instead; `#Resource` sets `type` and types `configuration` (see [Typed configuration](#typed-configuration)).

With or without generated types, `configuration` is also validated by the provider's own schema at plan time, including nested blocks.

The `infrastructure` block is closed at every level: a misspelled field such as `resource:` or `sourcee:` fails evaluation with `field not allowed` and its position, instead of being ignored (which would otherwise plan the deletion of everything under the real field). The schema also checks the references and the provider install choice with CUE's `error()`: a provider must set exactly one of `version` and `path`, every `dependsOn` entry must name a resource of the same configuration, every resource's provider (explicit, or the prefix of its `type`) must be declared in the same configuration, and the state `url` must be valid. The checks apply to the top level and to every `environments.NAME` configuration, **selected or not**, so a mistake in an environment you are not using is still found, and each error carries its field path (for example `infrastructure.environments.dev._unresolved."resources.pet.dependsOn[0]"`). A provider, resource or `configuration` that CUE itself rejects (a conflicting value, a `version` that is not exact) is reported by CUE alone, and the checks that read it wait until it is fixed, so one mistake does not bury the real error under false "no provider named" messages. They need no network and no provider. See [the schema reference](/reference/cue-schema/#checks).

The Rust engine repeats these checks before launching a provider, as defense in depth for projects that do not unify with cuenv's schema. It collects **every** problem rather than stopping at the first, each with its full field path, for example `infrastructure.environments.dev.resources.pet.dependsOn[0]`, and an environment's messages say that top-level providers are not inherited. Each dependency cycle is reported on its own, with only its member resources and the `dependsOn` entries that form it, even when other dependencies are unknown.

## Plan and apply

`cuenv i plan` refreshes every recorded resource from the real world, then asks each provider to plan:

```text
cuenv infrastructure: github.com/cuenv/cuenv#infrastructure-random
  + random_pet.pet (create)
      + id = (known after apply)
      + length = 2
      + separator = "-"
  -/+ random_integer.port (replace)
      # forced by: keepers
      ~ keepers: {"pet":"a"} -> {"pet":"b"}

Plan: 1 to create, 0 to update, 1 to replace, 0 to delete, 0 to refresh, 1 unchanged.
```

Values that are sensitive anywhere inside an attribute or nested block are shown as `(sensitive)`. "To refresh" counts resources whose provider-reported state or recorded metadata (dependencies, schema version) changed without any change to the resource itself; `apply` records them under the lock.

Every plan is checked the way Terraform checks it: a provider that plans a value contradicting your configuration, including inside nested blocks and nested attributes, is reported as a provider error naming the resource and attribute path, and nothing is applied.

`cuenv i apply`, like Terraform:

1. takes the project's lock (creating or upgrading cuenv's tables first if needed);
2. plans and shows the plan;
3. asks for confirmation while still holding the lock, so nothing can change between what you approve and what is applied — answering anything but `yes`, closing input or pressing Ctrl-C releases the lock and exits `1` (`130` for an interrupt);
4. applies that same plan one operation at a time, in the order the plan lists its changes, recording each result immediately. Every write is fenced by the lock, so a run whose lock was released or taken over cannot overwrite newer state.

`cuenv i apply --yes` (required when standard input is not a terminal; `--auto-approve` is accepted as an alias) does the same without asking. `destroy` behaves the same way.

Resources removed from `infrastructure.resources` are deleted. Stored state is only ever handed back to the provider source that created it.

### Apply order

Apply orders the plan as one dependency graph, following Terraform's rules, and the plan lists its changes in the order they run, so the preview is the order of events. A replacement is two operations in the graph: the delete of the old object and the create of the new one.

- A create or update waits for the create or update of every resource it is configured to depend on (`dependsOn`). A resource whose configuration only changes its `dependsOn` has its stored record rewritten (a refresh), and a resource that depends on it waits for that rewrite too.
- The create half of a replacement waits for its delete half.
- The delete of a resource waits for the deletes of every resource whose **stored** record depends on it: dependents go before what they depend on. This holds for resources removed from the configuration and for replacements.
- A create or update of a resource waits for the deletes of everything whose stored record depends on it, so an object that hangs off a resource is gone before that resource is changed, and for the deletes of everything its own stored record depends on (Terraform connects creators to the destroyers they may depend on).
- The creates of new resources, and the create halves of replacements, wait for the delete half of every replacement that is configured to depend on them: **a replacement's old object is deleted before the creates of its new prerequisites**. cuenv adds this on top of Terraform's rules, because the old object is destroyed anyway and a prerequisite that is created first may take over an identity the old object holds (the same `filename`, the same remote name): it would fail with "already exists", or succeed and then lose the object to the old object's delete. The price is availability: if such a create fails, the replacement is left deleted and not recreated (reported, as for a replaced child of a parent whose update fails), and the next successful apply creates it. An update is not held back, since it keeps its own object. No delete waits for anything but deletes, so this never makes the delete of a removed resource wait for a create.
- Among the operations that may run, the deletes of removed resources (and the replacement deletes that must precede them) go first, then the other deletes, then refreshes, then creates and updates. After the delete half of a replacement, its create and whatever the create still waits for go next, so the object is missing for as short a time as the graph allows. **Nothing makes the delete of a removed resource wait for a create, update or refresh**, so renaming a resource key whose real object keeps the same identity (for example a `local_file` with the same `filename`) deletes the old object before the new one is created, whether or not other resources depend on it and follow the rename.

Removing a resource and the `dependsOn` that points at it in one change follows Terraform's order: the removed resource is deleted first and the dependent is updated afterwards. A provider that refuses to delete an object that something still uses stops the apply at that delete (the dependent's update is skipped, nothing is lost); make the change in two applies, first detaching the dependent while the resource stays declared, then removing the resource.

cuenv has no `moved` block, so renaming a resource key is a delete of the old address followed by a create of the new one. Renaming a parent whose children still exist therefore fails with a provider that refuses to delete an object that is still in use: the delete is refused, the new object is not created while the old one holds its identity, and nothing is lost. Do it in two applies, as above: first detach the children (remove their `dependsOn` and whatever else names the parent) while the parent keeps its old name, then rename the parent and attach the children again.

Dependencies are recorded in state as full `type.name` addresses. Dependency cycles in the configuration, and changes that no order can apply, are refused while planning, before any confirmation and before any provider changes anything. Stored dependencies are history rather than configuration: records that depend on each other (an earlier failed apply can leave that behind) do not block `plan`, `apply` or `destroy`; the dependency that would close the cycle is ignored when ordering deletes, with a warning. Each completed operation is recorded before the next one starts.

When a provider fails an operation, every operation that depends on it is skipped, and every other operation still runs. A replacement whose delete has not run yet is not started when its create can no longer run; one whose old object was already deleted (see the rules above) is reported as deleted and not recreated. The command then ends with exit code `5` and one error that lists the failures, the skipped changes and the replacements that were deleted but not recreated. An interrupted run can also leave a replacement deleted and not recreated.

:::caution[Deleted but not recreated]
A replacement whose old object was destroyed but whose new object was not created leaves that object missing until the next apply creates it. cuenv reports every such address on every way a run can end: a warning as it happens, the error's help text, and `deletedNotRecreated` (the addresses) in the JSON error envelope. Run `apply` again to create them. If the provider deleted the old object but recording the deletion failed (for example the lock was lost), the address is reported the same way and the next plan sees the object gone and creates it. If the new object was created but could not be recorded (a recovery file was saved), the address is **not** reported as missing: it exists, the warning says so and the error names the recovery file.
:::

### When things go wrong

- **Interrupts.** The first Ctrl-C, SIGTERM, SIGHUP or SIGQUIT asks every running provider to stop, as Terraform does: no new resource is started, the operation in flight returns early, and whatever it returns is recorded (an interrupted create is recorded as tainted); then the lock is released. A signal that arrives while the last operation is in flight lets it finish and be recorded, and the run still ends as interrupted (exit `130`, `infrastructure_interrupted`, with what was applied in the message and, with `--json`, in `result`). A second signal kills the providers, waits up to two seconds for a record being written, releases the lock if it can within two seconds, prints the lock identifier (in JSON mode, as the single error document with `lockIdentifier` and `lockReleased`) and exits `130`. Continuous integration cancellation (SIGINT, then SIGTERM about 7.5 seconds later on GitHub Actions) records the resource in flight when its provider honours the stop within that window; otherwise the second signal kills it and the next plan refreshes whatever exists. Providers run in their own process group, so a terminal Ctrl-C reaches only cuenv; the whole group is killed on a forced exit, and on Linux providers also die if cuenv itself is killed.
- **Partial failures.** If a create fails after the provider made something, or returns values it never resolved, the result is recorded as **tainted** and the next plan replaces it. A failed update or delete keeps the stored taint. `cuenv i state` marks tainted resources.
- **State store outages.** If the provider changed a resource but the change cannot be recorded (after retries), cuenv saves the new state under your user state directory (`~/.local/state/cuenv/infrastructure/unrecorded/` on Linux, readable only by you, never inside the project) and tells you, instead of silently forgetting a real resource. `plan`, `apply` and `destroy` refuse to run until `cuenv i state recover` has recorded those files. On an ephemeral continuous integration runner the directory disappears with the runner, so fix the state store and re-run on the same machine where possible. Errors never include state values.
- **Stale locks.** Every run prints `Acquired lock <identifier>` on standard error, and `Released lock <identifier>` when it ends and the lock was still held. If the lock row was removed while the run was working (another actor released it with `unlock`, or deleted it), the run says the lock was no longer held and that nothing was released, instead of claiming a release; a failure then reports `lockReleased: false`. `cuenv i unlock` shows who holds the lock and since when; `cuenv i unlock <lock identifier>` releases exactly that lock. Locks are per identity (project and environment), so pass the same `--env` as the run that holds it. `unlock <identifier>` never succeeds without releasing something: if the identifier matches no lock (it was released already, or belongs to another environment, which the error says), it exits `2`, and if another run holds the lock it exits `4`. A lock left behind by a project that no longer evaluates, was deleted, or lives in another repository sharing the database is found with `cuenv i state locks`, which lists every lock in the database (module, project, environment, identifier, holder, age) with the command that releases it, and released with `cuenv i unlock <lock identifier> --module <module path> --project <project> [--env <name>]`. That command evaluates the current project only to learn which database to reach; it does not evaluate the locked project. Check that the run is gone before releasing a lock.
- **Recovery conflicts.** `cuenv i state recover` compares the stored generation and serial with the version the saved change replaced. Deleting and recreating an address starts a fresh generation even when its contents and serial match. A retry acknowledges only the same generation, resulting serial and contents. If another run changed the record, recovery stops and names the file, the address and the reason: inspect both objects, then either move the saved file aside or run `cuenv i state recover --force` to record it anyway.
- **Removed providers and damaged records.** `cuenv i state remove <address>` forgets one managed resource without touching the real object, under the lock — the escape hatch when its provider is gone. It reads only the address columns, so it also removes a record whose content cannot be decoded (damaged, or not written by cuenv); `state list` and `plan` report such a record by address and name this command.
- **Changed state backend.** Recovery files bind to a hash of the normalized Turso backend URL (`localhost`, `127.0.0.1` and `[::1]` count as the same backend, since they can only reach this machine on the same port). Recovery refuses a file saved for a different backend, or without a binding, and names the file. `--force` does not override that. After inspecting the saved object and both backends, run `cuenv i state recover --accept-backend`. The two flags are independent: `--force` overrides a changed stored record only, `--accept-backend` overrides a backend mismatch only, and each is decided file by file.
- **Provider executable busy.** Linux refuses to start a program that some process still has open for writing (`Text file busy`). A provider cuenv has just installed can be in that state for a moment when another process was started while the file was being written, so cuenv retries the launch for about a second before reporting the error.
- **Lost provider response during interruption.** If the apply RPC ends without a response, cuenv names the resource whose outcome is unknown. Inspect the provider before retrying: the operation may have changed infrastructure without returning state to record.
- **Exit codes.** `1` you declined the confirmation, `2` configuration (including a duplicate project name and the refusals described in [Moving between identities](#moving-between-identities)), `3` evaluation (including any instance in the module that cannot be evaluated, and a secret that cannot be resolved), `4` another run holds the lock, or a schema migration is blocked by held locks or waiting for them (retry later), `5` other infrastructure failures (including state owned by another instance), `130` interrupted. JSON error codes are `infrastructure`, `infrastructure_locked`, `infrastructure_cancelled` and `infrastructure_interrupted`; every error document carries `help`, and lock-related ones carry `lockIdentifier` and `lockReleased`.

## Current limitations

- **No references between resources.** A resource cannot consume another's computed attributes; use `dependsOn` for ordering only.
- **Resource identity is not supported.** Plugin Framework resources that declare an identity (recent AWS, Google and Azure resources) fail on update with "Missing Resource Identity After Update".
- **Dynamic-typed attributes** round-trip as tuples and objects rather than their original list, set or map types.
- No data sources, imports, `moved` blocks, saved plan files or `--target`.
- Replacement is always destroy-then-create; operations run one at a time.
- **No `state move`.** Records cannot be moved between the no-flag identity and a named environment, or between environment names; see [Moving between identities](#moving-between-identities).
- **One state database for all environments.** `state` cannot be overridden per environment.
- **Semantic errors are fail-closed.** A mistake the schema's checks report (see [the schema reference](/reference/cue-schema/#checks)) in _any_ environment, selected or not, is a CUE evaluation error (exit code `3`) and fails every command that evaluates the project, including ordinary `cuenv env`, `cuenv task`, `cuenv sync` and `cuenv fmt` runs and the shell hook (`cuenv export --shell`, which runs on every prompt). State-only commands (`state list`, `unlock` and the others) fail too, even though they need only the state backend: fix the configuration first.
- Provider version constraints and GPG signature verification are not implemented. Lockfile pinning needs `cuenv infrastructure provider add`; providers without a pin are verified against the registry's own checksum only.
- **Typed configuration** covers managed resources and provider blocks only; local `path` providers cannot be typed yet.
