---
title: Manage infrastructure
description: Declare managed resources in CUE and let cuenv drive Terraform provider plugins directly, with multi-tenant state in Turso.
---

cuenv can manage real infrastructure with the provider ecosystem you already know from Terraform and OpenTofu — without either CLI. `cuenv infra` launches unmodified `terraform-provider-*` binaries, speaks their gRPC plugin protocol (versions 5 and 6), and stores every managed resource as its own record in a remote [Turso](https://turso.tech) (libSQL) database.

:::caution[Status: proof of concept]
`#Infra` is **partial**. Create, update, replace, delete, refresh, state upgrades, dependency ordering, state locking and registry installs work against real providers. Resources **cannot reference each other's attributes yet** (no "known after apply" values flowing between resources), and there are no data sources, imports, saved plans, or parallel applies. Check [Schema status](/reference/schema/status/) before relying on a capability.
:::

## How state is keyed

State is multi-tenant by construction. Every record is keyed by:

| Key | Source | Example |
| --- | --- | --- |
| Tenant | `module:` in `cue.mod/module.cue`, without the `@vN` suffix | `github.com/acme/platform` |
| Discriminator | the project's `name` | `web` |
| Address | resource `type` and its name in `infra.resources` | `random_pet.server` |

cuenv refuses to plan or apply without a CUE module path, so many modules and projects can safely share one database. Moving a project to a different module or renaming it starts from empty state.

## A minimal example

This is [`examples/infra-random`](https://github.com/cuenv/cuenv/tree/main/examples/infra-random):

```cue
package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "infra-random"

infra: {
	state: turso: url: "http://127.0.0.1:8080"

	providers: random: {
		source:  "hashicorp/random"
		version: "3.7.2"
	}

	resources: {
		pet: {
			type: "random_pet"
			config: {
				length:    2
				separator: "-"
			}
		}
		db_password: {
			type: "random_password"
			config: {
				length:  24
				special: false
			}
		}
		port: {
			type:      "random_integer"
			dependsOn: ["pet"]
			config: {
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
cuenv infra plan  -p examples/infra-random --package examples
cuenv infra apply -p examples/infra-random --package examples
cuenv infra state -p examples/infra-random --package examples
cuenv infra destroy -p examples/infra-random --package examples
```

## Configure the state database

```cue
infra: state: turso: {
	url:          "libsql://platform-acme.turso.io"
	authTokenEnv: "TURSO_AUTH_TOKEN" // default
}
```

The auth token is read from the named environment variable at run time and never written to CUE or state. Create one with `turso db tokens create <db>`. cuenv creates its two tables (`cuenv_infra_resources`, `cuenv_infra_locks`) on first use.

:::caution
Like Terraform state, resource records contain every attribute the provider returns, including values marked sensitive (for example `random_password.result`). Treat the database as secret material and scope its tokens accordingly.
:::

## Configure providers

```cue
infra: providers: {
	// Installed from registry.terraform.io into the plugin cache.
	random: {
		source:  "hashicorp/random"
		version: "3.7.2"
	}
	// A local binary, relative to the project directory.
	cloudflare: {
		source: "cloudflare/cloudflare"
		path:   "bin/terraform-provider-cloudflare_v5.26.0"
		config: api_token: "..." // provider block arguments
	}
}
```

- `source` is `namespace/type` or `hostname/namespace/type`.
- `version` must be exact; constraints (`~> 3.7`) are not supported.
- Downloads are verified against the registry's SHA-256 checksum and cached at `$TF_PLUGIN_CACHE_DIR` when set, otherwise `~/.cache/cuenv/infra/providers`, using Terraform's cache layout. The registry's GPG signature is not verified yet.
- `config` is the provider block. Providers also read their usual environment variables (`CLOUDFLARE_API_TOKEN`, `AWS_PROFILE`, …) from the environment `cuenv infra` runs in.

## Declare resources

```cue
infra: resources: web_dns: {
	type:      "cloudflare_dns_record"
	provider:  "cloudflare"     // defaults to the type prefix
	dependsOn: ["zone_settings"] // apply after, destroy before
	config: {
		zone_id: "..."
		name:    "www"
		type:    "CNAME"
		content: "example.pages.dev"
		ttl:     1
	}
}
```

`config` is validated by the provider's own schema at plan time, including nested blocks. Unknown arguments are rejected with the attribute path.

## Plan and apply

`cuenv infra plan` refreshes every recorded resource from the real world, then asks each provider to plan:

```text
cuenv infra: github.com/cuenv/cuenv#infra-random
  + random_pet.pet (create)
      + id = (known after apply)
      + length = 2
      + separator = "-"
  -/+ random_integer.port (replace)
      # forced by: keepers
      ~ keepers: {"pet":"a"} -> {"pet":"b"}

Plan: 1 to create, 0 to update, 1 to replace, 0 to delete, 1 unchanged.
```

`cuenv infra apply` re-plans under an exclusive lock and asks for confirmation; `--auto-approve` skips the prompt and is required when stdin is not a terminal. Resources removed from `infra.resources` are deleted. State is written after every resource, so a failed apply never loses track of what was created.

If a run is killed while holding the lock, release it with `cuenv infra unlock`.

## Current limitations

- No references between resources: a resource cannot consume another's computed attributes. Use `dependsOn` for ordering only.
- No data sources, imports, `moved` blocks, or saved plan files.
- Replacement is always destroy-then-create.
- Resources apply one at a time.
- Provider version constraints and GPG signature verification are not implemented.
- Proposed-state computation is a simplified port of Terraform's: computed attributes are carried from prior state through single nested blocks; collections of nested blocks are taken from configuration verbatim.
