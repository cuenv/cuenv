---
title: Manage infrastructure
description: Declare managed resources in CUE and let cuenv drive Terraform provider plugins directly, with typed provider schemas from the CUE registry and multi-tenant state in Turso.
---

cuenv can manage real infrastructure with the provider ecosystem you already know from Terraform and OpenTofu — without either command line tool. `cuenv infrastructure` (short form: `cuenv i`) launches unmodified `terraform-provider-*` binaries, speaks their gRPC plugin protocol (versions 5 and 6), and stores every managed resource as its own record in a remote [Turso](https://turso.tech) (libSQL) database.

:::caution[Status: proof of concept]
`#Infrastructure` is **partial**. Create, update, replace, delete, refresh, state upgrades, dependency ordering, fenced state locking and verified registry installs work against real providers. See [Current limitations](#current-limitations) before pointing it at anything you care about — in particular, Plugin Framework resources that declare a resource identity are not handled yet.
:::

## Commands

| Command | Short form | What it does |
| --- | --- | --- |
| `cuenv infrastructure plan` | `cuenv i plan` | Refresh recorded resources and show what `apply` would change |
| `cuenv infrastructure apply` | `cuenv i apply` | Lock, plan, confirm, apply |
| `cuenv infrastructure destroy` | `cuenv i destroy` | Delete every managed resource the project owns |
| `cuenv infrastructure state list` | `cuenv i state` | List managed resources recorded for the project (`list` is the default) |
| `cuenv infrastructure state remove <address>` | `cuenv i state remove` | Forget one managed resource without deleting it |
| `cuenv infrastructure state recover` | `cuenv i state recover` | Record changes that could not be recorded earlier |
| `cuenv infrastructure state adopt` | `cuenv i state adopt` | Make this instance the owner of the project's state |
| `cuenv infrastructure unlock` | `cuenv i unlock` | Show who holds the lock, or release it by identifier |

`i` is the only short form. Everything else — commands, schema definitions, fields — is spelled out in full. Every subcommand honours the global `--json` flag and then prints exactly one JSON document on standard output (events go to standard error); `apply` and `destroy` require `--yes` with `--json`.

## How state is keyed

State is multi-tenant by construction. Every record is keyed by:

| Key | Source | Example |
| --- | --- | --- |
| Tenant | `module:` in `cue.mod/module.cue`, without the `@vN` suffix | `github.com/acme/platform` |
| Discriminator | the project's `name` | `web` |
| Address | resource `type` and its name in `infrastructure.resources` | `random_pet.server` |

cuenv refuses to run without a CUE module path, and `plan`, `apply` and `destroy` refuse to run when another instance anywhere in the module — any directory, any CUE package — has the same `name` and an `infrastructure` block; otherwise the two would share state and each would plan to delete the other's resources. CUE instances in the same package inherit fields from their parent directories, so a child directory shares its parent's `name` (it cannot set its own) and, if it inherits the `infrastructure` block too, is a conflict; put the child in a different CUE package or move its files. The check evaluates every instance in the module and fails closed: if any instance cannot be evaluated, the command stops and names it, and a target the module walk cannot see (a directory starting with `_` or `.`, a `testdata` directory, a nested module) is refused with the reason.

The state database also records which instance (`<directory>:<package>`) owns each project, claimed by the first `apply` or `destroy`. Any other instance using the same module path and project name — another checkout, a nested module, a copied directory — is refused until you move ownership explicitly with `cuenv i state adopt`, run from the instance that should own it. `state` and `unlock` skip both checks so a broken sibling never blocks recovery. Moving a project to a different module or renaming it starts from empty state.

:::caution[Tenancy is a naming boundary, not a security boundary]
The module path is declared by the project itself. Anyone holding a database token can read or write every tenant in that database. For isolation between teams or customers, give each tenant its own Turso database and token.
:::

## Typed provider schemas from the CUE registry

Every provider listed in [cuenv/terraform](https://github.com/cuenv/terraform) is published to the CUE registry as a module generated from the provider's own schema:

```text
github.com/cuenv/terraform/terraform/<namespace>/<type>@v<provider major version>
```

Each module exposes closed definitions: `#ProviderConfig` for the provider block and `#Resource_<type>` for every managed resource (plus `#DataSource_<type>` and others for future use). Unify them with `configuration` and evaluation fails — naming the field, and usually its file position — on unknown arguments, wrong types, missing required arguments and values that are not concrete, before any provider is started.

Pin the exact provider release in `cue.mod/module.cue`:

```bash
cue mod get github.com/cuenv/terraform/terraform/hashicorp/random@v3.9.1
```

```cue
deps: "github.com/cuenv/terraform/terraform/hashicorp/random@v3": v: "v3.9.1"
```

Then import it under an alias that matches **no** field name in scope — not a provider key, not a resource key, not `state`, `providers`, `resources` or `name`:

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
		version:       "3.9.1" // same release as the pinned module
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

CUE resolves an identifier to the nearest enclosing field of that name before an import, so `random.#ProviderConfig` written inside `providers: random: {...}` refers to the field. cuenv reports that as `infrastructure.providers.random.configuration: undefined field: #ProviderConfig` with its position; an alias ending in `Provider` avoids it.

Two more rules:

- **Package names.** The module's package is the provider type with characters that are not letters, digits or `_` replaced by `_`, and the result prefixed with `provider_` when it is a reserved CUE word or does not start with a letter. `hashicorp/google-beta` is package `google_beta`, which differs from the last path element, so the import needs the qualifier: `googleBetaProvider "github.com/cuenv/terraform/terraform/hashicorp/google-beta@v8:google_beta"`; `hashicorp/null` is package `provider_null`, so import it with an explicit qualifier: `nullProvider "github.com/cuenv/terraform/terraform/hashicorp/null@v3:provider_null"`.
- **Versions.** Keep `version` equal to the release pinned in `deps`. Nothing enforces this yet; a mismatch means the schema you typed against is not the one the provider uses.

## A minimal example without typed schemas

[`examples/infrastructure-random`](https://github.com/cuenv/cuenv/tree/main/examples/infrastructure-random) uses untyped `configuration` so it evaluates without network access. The provider still validates every argument at plan time.

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

## Configure the state database

```cue
infrastructure: state: turso: {
	url: "libsql://platform-acme.turso.io"
	authenticationTokenEnvironmentVariable: "TURSO_AUTH_TOKEN" // default
}
```

- `url` accepts `libsql://`, `https://` and `wss://`. Plain `http://` and `ws://` are accepted only for loopback addresses (a local `sqld`), so the token never crosses a network in cleartext. URLs must not carry credentials, queries or fragments.
- The authentication token is read from the named environment variable at run time. It is never written to CUE or state, never shown in errors or logs, and **withheld from provider processes**. Create one with `turso db tokens create <database>`.
- cuenv creates and migrates its tables (`cuenv_infrastructure_schema`, `cuenv_infrastructure_resources`, `cuenv_infrastructure_locks`) right before any command takes the lock. Other commands (`plan`, `state list`, `unlock`, `state recover` with nothing to recover) never create or migrate tables, so they work with a read-only token and report empty state for a fresh database. A database migrated by a newer cuenv is refused rather than misread.
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
- Every resource's provider — its explicit `provider`, or the prefix of its `type` — must be declared here; evaluation fails otherwise. `dependsOn` entries must name declared resources.
- Set exactly one of `version` (an exact semantic version; constraints such as `~> 3.7` are not supported) or `path`.
- Downloads must be HTTPS, are verified against the registry's SHA-256 checksum, and are cached using Terraform's layout in `$TF_PLUGIN_CACHE_DIR` when set, otherwise in your platform's cache directory under `cuenv/infrastructure/providers` (`~/.cache` on Linux, `~/Library/Caches` on macOS). cuenv records a manifest with the binary's SHA-256 and re-verifies it on every use; a cache populated by Terraform is reinstalled once. That manifest only detects accidental corruption: anyone who can write to the cache can replace a binary and its manifest together, so never share a writable plugin cache between trust boundaries. The registry's GPG signature and lockfile hashes are not verified yet.
- `configuration` is the provider block. Keep credentials out of it: providers read their usual environment variables (`CLOUDFLARE_API_TOKEN`, `AWS_PROFILE`, …) from the environment `cuenv infrastructure` runs in. Secret-typed arguments are not supported yet.

## Declare resources

```cue
infrastructure: resources: {
	zone_settings: {
		type: "cloudflare_zone_setting"
		configuration: cloudflareProvider.#Resource_cloudflare_zone_setting & {
			zone_id:    "..."
			setting_id: "always_use_https"
			value:      "on"
		}
	}
	web_dns: {
		type:      "cloudflare_dns_record"
		provider:  "cloudflare"     // defaults to the type prefix
		dependsOn: ["zone_settings"] // must be declared; apply after, destroy before
		configuration: cloudflareProvider.#Resource_cloudflare_dns_record & {
			zone_id: "..."
			name:    "www"
			type:    "CNAME"
			content: "example.pages.dev"
			ttl:     1
		}
	}
}
```

With or without a registry definition, `configuration` is also validated by the provider's own schema at plan time, including nested blocks.

The `infrastructure` block is closed at every level: a misspelled field such as `resource:` or `sourcee:` fails evaluation with `field not allowed` and its position, instead of being ignored (which would otherwise plan the deletion of everything under the real field). A `dependsOn` entry or provider that is not declared fails with a message naming it, for example `no resource named "pett" in infrastructure.resources`.

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
4. applies that same plan one resource at a time, recording each result immediately. Every write is fenced by the lock, so a run whose lock was released or taken over cannot overwrite newer state.

`cuenv i apply --yes` (required when standard input is not a terminal; `--auto-approve` is accepted as an alias) does the same without asking. `destroy` behaves the same way.

Resources removed from `infrastructure.resources` are deleted. Stored state is only ever handed back to the provider source that created it.

### When things go wrong

- **Interrupts.** The first Ctrl-C, SIGTERM, SIGHUP or SIGQUIT asks every running provider to stop, as Terraform does: no new resource is started, the operation in flight returns early, and whatever it returns is recorded (an interrupted create is recorded as tainted); then the lock is released. A second signal kills the providers, waits up to two seconds for a record being written, releases the lock if it can within two seconds, prints the lock identifier (in JSON mode, as the single error document with `lockIdentifier` and `lockReleased`) and exits `130`. Continuous integration cancellation (SIGINT, then SIGTERM about 7.5 seconds later on GitHub Actions) records the resource in flight when its provider honours the stop within that window; otherwise the second signal kills it and the next plan refreshes whatever exists. Providers run in their own process group, so a terminal Ctrl-C reaches only cuenv; the whole group is killed on a forced exit, and on Linux providers also die if cuenv itself is killed.
- **Partial failures.** If a create fails after the provider made something, or returns values it never resolved, the result is recorded as **tainted** and the next plan replaces it. A failed update or delete keeps the stored taint. `cuenv i state` marks tainted resources.
- **State store outages.** If the provider changed a resource but the change cannot be recorded (after retries), cuenv saves the new state under your user state directory (`~/.local/state/cuenv/infrastructure/unrecorded/` on Linux, readable only by you, never inside the project) and tells you, instead of silently forgetting a real resource. `plan`, `apply` and `destroy` refuse to run until `cuenv i state recover` has recorded those files. On an ephemeral continuous integration runner the directory disappears with the runner, so fix the state store and re-run on the same machine where possible. Errors never include state values.
- **Stale locks.** Every run prints `Acquired lock <identifier>` on standard error. `cuenv i unlock` shows who holds the lock and since when; `cuenv i unlock <lock identifier>` releases exactly that lock.
- **Recovery conflicts.** `cuenv i state recover` only records a saved change if the stored record is still the version the change replaced. If another run changed it in the meantime, recovery stops and names the address: inspect both objects, then either move the saved file aside or run `cuenv i state recover --force` to record it anyway.
- **Removed providers.** `cuenv i state remove <address>` forgets one managed resource without touching the real object — the escape hatch when its provider is gone.
- **Exit codes.** `1` you declined the confirmation, `2` configuration (including a project owned by another instance), `3` evaluation (including any instance in the module that cannot be evaluated), `4` another run holds the lock (retry later), `5` other infrastructure failures, `130` interrupted. JSON error codes are `infrastructure`, `infrastructure_locked`, `infrastructure_cancelled` and `infrastructure_interrupted`; every error document carries `help`, and lock-related ones carry `lockIdentifier` and `lockReleased`.

## Current limitations

- **No references between resources.** A resource cannot consume another's computed attributes; use `dependsOn` for ordering only.
- **Resource identity is not supported.** Plugin Framework resources that declare an identity (recent AWS, Google and Azure resources) fail on update with "Missing Resource Identity After Update".
- **Dynamic-typed attributes** round-trip as tuples and objects rather than their original list, set or map types.
- No data sources, imports, `moved` blocks, saved plan files or `--target`.
- Replacement is always destroy-then-create; resources apply one at a time.
- Provider version constraints, lockfile pinning and GPG signature verification are not implemented.
