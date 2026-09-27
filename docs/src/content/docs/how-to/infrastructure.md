---
title: Manage infrastructure
description: Declare managed resources in CUE and let cuenv drive Terraform provider plugins directly, with typed provider schemas from the CUE registry and multi-tenant state in Turso.
---

cuenv can manage real infrastructure with the provider ecosystem you already know from Terraform and OpenTofu — without either command line tool. `cuenv infrastructure` (short form: `cuenv i`) launches unmodified `terraform-provider-*` binaries, speaks their gRPC plugin protocol (versions 5 and 6), and stores every managed resource as its own record in a remote [Turso](https://turso.tech) (libSQL) database.

:::caution[Status: proof of concept]
`#Infrastructure` is **partial**. Create, update, replace, delete, refresh, state upgrades, dependency ordering, state locking and registry installs work against real providers. Resources **cannot reference each other's attributes yet** (no "known after apply" values flowing between resources), and there are no data sources, imports, saved plans, or parallel applies. Check [Schema status](/reference/schema/status/) before relying on a capability.
:::

## Commands

| Command | Short form | What it does |
| --- | --- | --- |
| `cuenv infrastructure plan` | `cuenv i plan` | Refresh recorded resources and show what `apply` would change |
| `cuenv infrastructure apply` | `cuenv i apply` | Lock, re-plan, confirm, converge |
| `cuenv infrastructure destroy` | `cuenv i destroy` | Delete every managed resource the project owns |
| `cuenv infrastructure state` | `cuenv i state` | List managed resources recorded for the project |
| `cuenv infrastructure unlock` | `cuenv i unlock` | Release a lock left by an interrupted run |

`i` is the only short form. Everything else — commands, schema definitions, fields — is spelled out in full.

## How state is keyed

State is multi-tenant by construction. Every record is keyed by:

| Key | Source | Example |
| --- | --- | --- |
| Tenant | `module:` in `cue.mod/module.cue`, without the `@vN` suffix | `github.com/acme/platform` |
| Discriminator | the project's `name` | `web` |
| Address | resource `type` and its name in `infrastructure.resources` | `random_pet.server` |

cuenv refuses to plan or apply without a CUE module path. Moving a project to a different module or renaming it starts from empty state.

:::caution[Tenancy is a naming boundary, not a security boundary]
The module path is declared by the project itself. Anyone holding a database token can read or write every tenant in that database. For isolation between teams or customers, give each tenant its own Turso database and token.
:::

## Typed provider schemas from the CUE registry

Every provider listed in [cuenv/terraform](https://github.com/cuenv/terraform) is published to the CUE registry as a module generated from the provider's own schema:

```text
github.com/cuenv/terraform/terraform/<namespace>/<type>@v<provider major version>
```

Each module exposes closed definitions: `#ProviderConfig` for the provider block and `#Resource_<type>` for every managed resource (plus `#DataSource_<type>` and others for future use). Unify them with `configuration` and CUE rejects unknown arguments, wrong types, and missing required arguments before any provider is started.

Add the dependency to `cue.mod/module.cue` (or run `cue mod get github.com/cuenv/terraform/terraform/hashicorp/random@v3`):

```cue
deps: "github.com/cuenv/terraform/terraform/hashicorp/random@v3": v: "v3.9.1"
```

Then import it under an alias that does **not** match your provider's local name:

```cue
package cuenv

import (
	"github.com/cuenv/cuenv/schema"
	randomProvider "github.com/cuenv/terraform/terraform/hashicorp/random@v3"
)

schema.#Project

name: "web"

infrastructure: {
	state: turso: url: "libsql://platform-acme.turso.io"

	providers: random: {
		source:        "hashicorp/random"
		version:       "3.9.1" // same major version as the module import
		configuration: randomProvider.#ProviderConfig
	}

	resources: pet: {
		type: "random_pet"
		configuration: randomProvider.#Resource_random_pet & {
			length:    3
			separator: "_"
		}
	}
}
```

:::danger[Why the alias matters]
CUE resolves an identifier to the nearest enclosing field of that name before an import. Inside `providers: random: { ... }`, `random.#ProviderConfig` means the field `providers.random.#ProviderConfig`, which does not exist. cuenv currently reports that as `invalid type: null, expected a map` rather than naming the undefined field. Importing as `randomProvider` (or any name that is not a provider key) avoids it.
:::

Keep the provider `version` and the module's major version in step: `hashicorp/random` `3.9.1` pairs with `…/random@v3`. The registry publishes one module version per provider release, so pinning `v3.9.1` in `deps` gives you exactly that release's schema.

## A minimal example without typed schemas

[`examples/infrastructure-random`](https://github.com/cuenv/cuenv/tree/main/examples/infrastructure-random) uses untyped `configuration` so it evaluates without network access. The provider still validates every argument at plan time.

```cue
infrastructure: {
	state: turso: url: "http://127.0.0.1:8080"

	providers: random: {
		source:  "hashicorp/random"
		version: "3.7.2"
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

## Configure the state database

```cue
infrastructure: state: turso: {
	url: "libsql://platform-acme.turso.io"
	authenticationTokenEnvironmentVariable: "TURSO_AUTH_TOKEN" // default
}
```

The authentication token is read from the named environment variable at run time and never written to CUE or state. Create one with `turso db tokens create <database>`. cuenv creates its two tables (`cuenv_infrastructure_resources`, `cuenv_infrastructure_locks`) on first use.

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
		configuration: api_token: "..." // provider block arguments
	}
}
```

- `source` is `namespace/type` or `hostname/namespace/type`.
- `version` must be exact; constraints (`~> 3.7`) are not supported.
- Downloads are verified against the registry's SHA-256 checksum and cached at `$TF_PLUGIN_CACHE_DIR` when set, otherwise `~/.cache/cuenv/infrastructure/providers`, using Terraform's cache layout. The registry's GPG signature is not verified yet.
- `configuration` is the provider block. Providers also read their usual environment variables (`CLOUDFLARE_API_TOKEN`, `AWS_PROFILE`, …) from the environment `cuenv infrastructure` runs in.

## Declare resources

```cue
infrastructure: resources: web_dns: {
	type:      "cloudflare_dns_record"
	provider:  "cloudflare"     // defaults to the type prefix
	dependsOn: ["zone_settings"] // apply after, destroy before
	configuration: cloudflareProvider.#Resource_cloudflare_dns_record & {
		zone_id: "..."
		name:    "www"
		type:    "CNAME"
		content: "example.pages.dev"
		ttl:     1
	}
}
```

With or without a registry definition, `configuration` is also validated by the provider's own schema at plan time, including nested blocks. Unknown arguments are rejected with the attribute path.

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

Plan: 1 to create, 0 to update, 1 to replace, 0 to delete, 1 unchanged.
```

`cuenv i apply` re-plans under an exclusive lock and asks for confirmation; `--auto-approve` skips the prompt and is required when standard input is not a terminal. Resources removed from `infrastructure.resources` are deleted. State is written after every resource, so a failed apply never loses track of what was created.

If a run is killed while holding the lock, release it with `cuenv i unlock`.

## Current limitations

- No references between resources: a resource cannot consume another's computed attributes. Use `dependsOn` for ordering only.
- No data sources, imports, `moved` blocks, or saved plan files.
- Replacement is always destroy-then-create.
- Resources apply one at a time.
- Provider version constraints and GPG signature verification are not implemented.
- Proposed-state computation is a simplified port of Terraform's: computed attributes are carried from prior state through single nested blocks; collections of nested blocks are taken from configuration verbatim.
