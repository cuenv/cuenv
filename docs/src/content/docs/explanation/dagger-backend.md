---
title: Dagger Runtime
description: Run cuenv tasks in Dagger containers for reproducible, isolated, multi-stage pipelines
---

Imagine deleting the brittle shell of a `Makefile` that "works on my machine" and the
`Dockerfile` that quietly drifts from CI. With the Dagger runtime you describe a task once
in CUE — the image, the secrets it may read, the cache it may reuse, the container it
continues from — and cuenv runs it the same way on your laptop and in CI. One typed config,
zero "but it built locally."

The Dagger runtime is how cuenv runs a task inside a [Dagger](https://dagger.io) container
instead of on the host, with first-class support for **container chaining**, **secret
mounts**, and **persistent cache volumes**.

:::caution[Status: Partial]
The Dagger runtime (`#DaggerRuntime`) is **Partial**, per the
[schema status reference](/reference/schema/status/). The schema is the supported surface
and cuenv parses it today, but Dagger execution and downstream image
output-reference resolution remain incomplete. Treat this page as a description of the
**intended, supported configuration shape** — validate behaviour against your installed
build before depending on it in production, and prefer the host backend for critical paths
until the matrix promotes this to implemented.
:::

## The supported surface: `runtime: schema.#DaggerRuntime`

cuenv selects where a task runs through its `runtime` field, the same mechanism Nix, devenv,
container, OCI, and tools runtimes use. Dagger is one variant of that union. You can set a
runtime project-wide as the default and override it per task. See
[Runtimes](/how-to/runtimes/) for the full runtime model.

`#DaggerRuntime` (from `schema/runtime.cue`) accepts:

| Field     | Required          | Purpose                                                     |
| --------- | ----------------- | ----------------------------------------------------------- |
| `type`    | yes (fixed)       | Always `"dagger"`.                                          |
| `image`   | unless `from` set | Base container image, e.g. `"rust:1.75-slim"`.              |
| `from`    | optional          | Continue from a prior task's container instead of an image. |
| `secrets` | optional          | List of `#DaggerSecret` mounts (env var or file).           |
| `cache`   | optional          | List of `#DaggerCacheMount` persistent volumes.             |

### Per-task runtime

The smallest useful form: pick an image for a single task.

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "dagger-runtime-demo"

tasks: {
	build: schema.#Task & {
		command: "cargo"
		args: ["build", "--release"]
		description: "Build the release binary in a pinned Rust image"
		runtime: schema.#DaggerRuntime & {
			image: "rust:1.75-slim"
		}
	}

	test: schema.#Task & {
		command: "pytest"
		args: ["-v", "tests/"]
		description: "Run the test suite in a Python image"
		runtime: schema.#DaggerRuntime & {
			image: "python:3.11-slim"
		}
	}
}
```

### Project-wide runtime with per-task overrides

Set a default `runtime` once and let individual tasks override it. Tasks without a `runtime`
inherit the project default; tasks with their own `runtime` win.

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "dagger-runtime-default"

// Project-wide default: every task runs in Dagger on Alpine unless it overrides.
runtime: schema.#DaggerRuntime & {
	image: "alpine:latest"
}

tasks: {
	hello: schema.#Task & {
		command: "hostname"
		description: "Inherits the project-wide Alpine Dagger runtime"
	}

	pythonInfo: schema.#Task & {
		command: "python"
		args: ["-c", "import sys; print(f'Running Python {sys.version} in Dagger')"]
		description: "Overrides the default with a Python image"
		runtime: schema.#DaggerRuntime & {
			image: "python:3.11-slim"
		}
	}
}
```

## Container chaining with `from`

A multi-stage pipeline installs dependencies once and reuses the resulting container state
in later tasks. Use `from: "<prior task>"` instead of `image` to continue from the container
a previous task produced.

```cue
tasks: {
	stage1Setup: schema.#Task & {
		command: "sh"
		args: ["-c", "apk add --no-cache curl jq && echo 'Setup complete!'"]
		description: "Install curl and jq into an Alpine container"
		runtime: schema.#DaggerRuntime & {
			image: "alpine:latest"
		}
	}

	stage2UseTools: schema.#Task & {
		command: "sh"
		args: ["-c", "which curl && which jq && echo '{\"test\": 123}' | jq ."]
		description: "Use the tools installed in stage1"
		dependsOn: [stage1Setup]
		runtime: schema.#DaggerRuntime & {
			from: "stage1Setup"
		}
	}
}
```

When you use `from`:

- The referenced task must complete successfully first — declare it in `dependsOn`.
- You do not specify `image`; the base is the prior task's container state (installed
  packages, files, and environment all carry over).

## Cache volumes

Mount named cache volumes to persist data — package-manager caches, build artifacts —
across task runs. Volumes that share a `name` share data across tasks and invocations, so a
warm cache speeds up repeated builds.

```cue
tasks: {
	install: schema.#Task & {
		command: "pip"
		args: ["install", "-r", "requirements.txt"]
		description: "Install Python deps with a persistent pip cache"
		runtime: schema.#DaggerRuntime & {
			image: "python:3.11-slim"
			cache: [
				{path: "/root/.cache/pip", name: "pip-cache"},
			]
		}
	}

	build: schema.#Task & {
		command: "cargo"
		args: ["build"]
		description: "Reuse cargo registry and git caches across runs"
		runtime: schema.#DaggerRuntime & {
			image: "rust:1.75-slim"
			cache: [
				{path: "/root/.cargo/registry", name: "cargo-registry"},
				{path: "/root/.cargo/git", name: "cargo-git"},
			]
		}
	}
}
```

Each `#DaggerCacheMount` is `{ path: string, name: string }` — `path` is the mount point
inside the container, `name` is the shared volume identity.

## Secret mounts

Secrets are resolved by cuenv's secret resolvers and passed into the container without
exposing plaintext in logs. Each entry in `secrets` is a `#DaggerSecret`: give it a `name`,
choose `envVar` (expose as an environment variable) or `path` (mount as a file), and supply
a `resolver`.

The `resolver` field takes an `#ExecSecret`-shaped value — `{ resolver: "exec", command:
..., args: [...] }` — never a bare `#Secret`. See [Secrets](/how-to/secrets/) for the full
resolver model and the providers cuenv supports.

### As an environment variable

```cue
tasks: {
	deploy: schema.#Task & {
		command: "sh"
		args: ["-c", "curl -H \"Authorization: Bearer $DEPLOY_TOKEN\" https://api.example.com/deploy"]
		description: "Deploy with a token mounted as an env var"
		runtime: schema.#DaggerRuntime & {
			image: "alpine:latest"
			secrets: [
				{
					name:   "deploy-token"
					envVar: "DEPLOY_TOKEN"
					resolver: {
						resolver: "exec"
						command:  "op"
						args: ["read", "op://vault/deploy/token"]
					}
				},
			]
		}
	}
}
```

### As a mounted file

```cue
tasks: {
	publish: schema.#Task & {
		command: "npm"
		args: ["publish"]
		description: "Publish with an .npmrc mounted as a file"
		runtime: schema.#DaggerRuntime & {
			image: "node:20-alpine"
			secrets: [
				{
					name: "npmrc"
					path: "/root/.npmrc"
					resolver: {
						resolver: "exec"
						command:  "op"
						args: ["read", "op://vault/npm/npmrc"]
					}
				},
			]
		}
	}
}
```

## A complete multi-stage pipeline

Dependencies, caching, container chaining, and a secret-mounted deploy step, expressed under
`#DaggerRuntime`:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "dagger-pipeline"

env: {
	APP_NAME: "myapp"
}

tasks: {
	deps: schema.#Task & {
		command: "sh"
		args: ["-c", "pip install flask gunicorn && pip freeze > requirements.txt"]
		description: "Install Python dependencies with a pip cache"
		outputs: ["requirements.txt"]
		runtime: schema.#DaggerRuntime & {
			image: "python:3.11-slim"
			cache: [
				{path: "/root/.cache/pip", name: "pip-cache"},
			]
		}
	}

	test: schema.#Task & {
		command: "pytest"
		args: ["-v", "tests/"]
		description: "Run tests in the deps container"
		dependsOn: [deps]
		runtime: schema.#DaggerRuntime & {
			from: "deps"
		}
	}

	build: schema.#Task & {
		command: "sh"
		args: ["-c", "python -m py_compile app.py && echo 'Build successful'"]
		description: "Verify the build, continuing from the test container"
		dependsOn: [test]
		runtime: schema.#DaggerRuntime & {
			from: "test"
		}
	}

	deploy: schema.#Task & {
		command: "sh"
		args: ["-c", "curl -X POST -H \"Authorization: Bearer $DEPLOY_TOKEN\" https://api.example.com/deploy"]
		description: "Deploy with a securely mounted token"
		dependsOn: [build]
		runtime: schema.#DaggerRuntime & {
			image: "alpine:latest"
			secrets: [
				{
					name:   "deploy-token"
					envVar: "DEPLOY_TOKEN"
					resolver: {
						resolver: "exec"
						command:  "op"
						args: ["read", "op://vault/deploy/token"]
					}
				},
			]
		}
	}
}
```

Environment variables defined in `env` are available to the task's container alongside the
mounted secrets.

## Legacy (deprecated): `config.backend` and task-level `dagger`

:::danger[Deprecated]
The older API — a global `config.backend.type: "dagger"` plus a per-task `dagger: { ... }`
block — is **deprecated**. The schema comment on `#Task` states it plainly:
`// DEPRECATED: Use runtime: dagger: { ... } instead`. The corresponding
`#DaggerConfig`, `#DaggerSecret`, and `#DaggerCacheMount` definitions are retained only for
backward compatibility. Prefer `runtime: schema.#DaggerRuntime` in all new configuration.
:::

The deprecated form looks like this, and is shown only so you can recognise and migrate it:

```cue
// Deprecated — migrate to runtime: schema.#DaggerRuntime
config: {
	backend: {
		type: "dagger"
		options: {
			image: "alpine:latest"
		}
	}
}

tasks: {
	build: schema.#Task & {
		command: "cargo"
		args: ["build", "--release"]
		dagger: {
			image: "rust:1.75-slim"
		}
	}
}
```

### Migrating to the runtime form

The field names inside the block are the same (`image`, `from`, `secrets`, `cache`), so
migration is mechanical:

- Replace a per-task `dagger: { ... }` block with `runtime: schema.#DaggerRuntime & { ... }`.
- Replace a global `config.backend.type: "dagger"` (with `options.image`) by setting a
  project-wide `runtime: schema.#DaggerRuntime & { image: ... }` and letting tasks inherit it.

## Known discrepancy: the example still uses the legacy form

`examples/dagger-task/env.cue` currently uses the deprecated `config.backend` plus
per-task `dagger: {}` API, not `runtime: schema.#DaggerRuntime`. That example predates this
guidance and should be migrated to the runtime form. We are flagging this honestly: until
that example is updated, do not treat it as the recommended pattern — follow the
`#DaggerRuntime` examples on this page instead.

## CLI backend override

You can force a backend at the command line, which is handy for debugging a Dagger task on
the host:

```bash
# Run on the host instead of Dagger (debugging)
cuenv task build --backend host

# Explicitly request Dagger
cuenv task build --backend dagger
```

## See also

- [Runtimes](/how-to/runtimes/) — the full runtime model and every `#Runtime` variant.
- [Run tasks](/how-to/run-tasks/) — task definitions, dependencies, and execution.
- [Secrets](/how-to/secrets/) — secret resolvers and supported providers.
- [Configure a project](/how-to/configure-a-project/) — general cuenv configuration.
- [Schema status](/reference/schema/status/) — authoritative implementation status.
