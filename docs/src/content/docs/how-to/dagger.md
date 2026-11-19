---
title: Dagger 1.0 Backend & Ephemeral Services
description: Run cuenv tasks and CI pipelines on the Dagger 1.0 Engine, generate Dagger 1.0 Dang workspaces, and use task-scoped ephemeral services
---

cuenv supports **Dagger 1.0** as a first-class execution backend for `cuenv task`
and `cuenv ci`, as well as a CI sync/export provider that emits Dagger 1.0
workspaces (`dagger.toml`, `dagger-module.toml`, and Dang `main.dang` modules).

In addition, `#Task.dependsOn` and `#TaskGroup.dependsOn` accept `#Service`
references directly:

- On the **Dagger backend**, referenced services are attached to the task
  container with `withServiceBinding`.
- On the **host backend**, cuenv starts **task-scoped ephemeral services** via
  `cuenv-services`, waits for their readiness probes, runs the task, and shuts
  them down automatically on completion, failure, or timeout.

## Configuring `#DaggerRuntime` or `#ContainerRuntime`

Declare a `#DaggerRuntime` (or the simpler `#ContainerRuntime`) in `env.cue`:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "my-app"

runtime: schema.#DaggerRuntime & {
	image:   "rust:1.100-slim"
	workdir: "/work"
	cacheVolumes: [
		{key: "cargo-registry", path: "/usr/local/cargo/registry", sharing: "SHARED"},
		{key: "cargo-target", path: "/work/target", sharing: "LOCKED"},
	]
}

services: {
	postgres: schema.#Service & {
		entrypoint: schema.#Command & {
			command: "postgres"
		}
		ports: [{name: "pg", port: 5432}]
		readiness: {
			kind: "port"
			port: 5432
		}
	}
}

let _services = services
let _tasks = tasks

tasks: {
	build: schema.#Task & {
		command: "cargo"
		args: ["build", "--workspace"]
		outputs: ["target/debug/app"]
	}

	integration: schema.#Task & {
		command: "cargo"
		args: ["test", "--test", "integration"]
		dependsOn: [_tasks.build, _services.postgres]
	}
}
```

`#DaggerRuntime` accepts either `image` or `dockerfile` (mutually exclusive),
optional `platform`, `workdir` (defaults to `"/work"`), `cacheVolumes`,
`engine` (`"local"`, `"cloud"`, or a remote engine URI), and
`disableDaggerInDagger`.

## Selecting the Execution Backend

Both `cuenv task` and `cuenv ci` accept `--backend` and `--dagger-engine`:

```bash
# Default: route tasks with container/dagger runtime to Dagger, others to host
cuenv task integration

# Force execution on the host (starts ephemeral services for #Service deps)
cuenv task --backend host integration

# Force execution on Dagger, targeting Dagger Cloud or a remote engine
cuenv task --backend dagger --dagger-engine cloud integration
cuenv ci --backend dagger --dagger-engine local
```

When running inside an existing Dagger session (`DAGGER_SESSION_PORT` and
`DAGGER_SESSION_TOKEN` are set), cuenv attaches to that session directly.
Otherwise it spawns `dagger session` lazily on the first Dagger-handled task and
reuses a single session across the entire task graph or CI pipeline.

### Caching Model

Tasks executed on the Dagger backend bypass `cuenv-cas` and local execution
roots (emitting `CacheSkipReason::ExternalBackend { backend: "dagger" }`) and
rely on Dagger Engine caching, declared `cacheVolumes`, and Dagger Cloud
distributed cache.

Secrets declared in `env` are passed via Dagger `setSecret` +
`withSecretVariable` so they are never baked into container layers or logs.

## Task-Scoped Ephemeral Services (Host Backend)

When a host-executed task (or task group) lists one or more `#Service` nodes in
`dependsOn`:

1. cuenv starts the referenced services (and their transitive service
   dependencies) in an isolated ephemeral session directory.
2. cuenv waits for each service's configured readiness probe (`port`, `http`,
   `log`, `command`, or `delay`).
3. The task runs on the host with `cuenv-cas` skipped (because the task depends
   on live service state).
4. Once the task completes, fails, or times out, cuenv tears down all ephemeral
   services and cleans up the session directory.

## Generating a Dagger 1.0 Workspace (`cuenv sync ci` & `--export`)

Add `"dagger"` to `ci.providers` in `env.cue`:

```cue
ci: {
	providers: ["dagger"]
	provider: dagger: {
		defaultImage: "rust:1.100-slim"
		moduleName:   "my-app"
	}
	pipelines: {
		default: {
			mode: "expanded"
			tasks: [_tasks.build, _tasks.integration]
		}
	}
}
```

Then sync or export:

```bash
# Emit dagger.toml, .dagger/modules/<name>/dagger-module.toml, and main.dang
cuenv sync ci --provider dagger

# Check for drift in CI
cuenv sync ci --provider dagger --check

# Print the generated Dang module to stdout
cuenv ci --export dagger
```

Generated Dang modules expose pipeline tasks as `@check` functions and project
services as `@up` services, ready for `dagger check` and `dagger up`.
