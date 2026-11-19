---
title: Dagger 1.0 Backend & Ephemeral Services
description: Run cuenv tasks and CI pipelines on the Dagger 1.0 Engine, generate Dagger 1.0 Dang workspaces, and use task-scoped ephemeral services
---

cuenv supports the **Dagger 1.0 beta** as an execution backend for `cuenv task`
and `cuenv ci`, as well as a CI sync/export provider that emits Dagger 1.0
workspaces (`dagger.toml`, `dagger-module.toml`, and Dang `main.dang` modules).
Use Dagger `v1.0.0-beta.28` for the session and workspace formats shown here.

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

name: "dagger-basic"

runtime: schema.#DaggerRuntime & {
	image:   "postgres:17-alpine"
	workdir: "/work"
}

services: {
	postgres: schema.#Service & {
		entrypoint: schema.#Command & {
			command: "docker-entrypoint.sh"
			args: ["postgres", "-c", "listen_addresses=*"]
		}
		runtime: schema.#ContainerRuntime & {
			image: "postgres:17-alpine"
		}
		env: {
			POSTGRES_HOST_AUTH_METHOD: "trust"
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
		command: "sh"
		args: ["-ec", "mkdir -p build; printf 'SELECT 1;\\n' > build/query.sql"]
		outputs: ["build/query.sql"]
	}

	integration: schema.#Task & {
		command: "psql"
		args: ["-h", "postgres", "-U", "postgres", "-v", "ON_ERROR_STOP=1", "-f", "build/query.sql"]
		dependsOn: [_tasks.build, _services.postgres]
		inputs: [_tasks.build.outputFiles]
	}
}
```

`#DaggerRuntime` accepts either `image` or `dockerfile` (mutually exclusive),
optional `platform`, `workdir` (defaults to `"/work"`), `cacheVolumes`,
`engine` (`"local"`, `"cloud"`, or a remote engine URI), and
`disableDaggerInDagger`.

Dagger services use an OCI image and execute their declared entrypoint
directly. Declare TCP readiness through `ports` and the optional `port`
probe so Dagger can wait for the service in its container network. Use
`docker-entrypoint.sh` explicitly if the image needs it for initialization.

## Selecting the Execution Backend

Both `cuenv task` and `cuenv ci` accept `--backend` and `--dagger-engine`:

```bash
# Default: route tasks with container/dagger runtime to Dagger, others to host
cuenv task integration

# Run a task and its host-native services on the host
cuenv task --backend host <task>

# Force execution on Dagger, targeting Dagger Cloud or a remote engine
cuenv task --backend dagger --dagger-engine cloud integration
cuenv ci --backend dagger --dagger-engine local
```

An explicit `--dagger-engine` takes precedence over the effective Dagger runtime’s
`engine` setting. Each target has its own session, opened when a task first uses it.
Without either override, cuenv reuses an inherited session supplied by
`DAGGER_SESSION_PORT` and `DAGGER_SESSION_TOKEN`, or starts `dagger session`
using the Dagger CLI’s default engine selection.

### Caching Model

The Dagger Engine caches container execution and provides the declared
`cacheVolumes`. With directory isolation enabled, cuenv stages only declared
inputs before mounting the task workspace. Declared outputs, including glob
patterns, are projected back into the host workspace after a successful run.
Put declared outputs outside cache volumes. A Dockerfile builds from the
project context, while the task itself reads the staged workspace.

cuenv resolves project and task variables before starting the container. It
passes secret variables through Dagger’s secret API and registers their values
for log redaction. Supply runtime tools in the container image. Host runtime
activation and host lifecycle hooks apply to host tasks.

## Task-Scoped Ephemeral Services (Host Backend)

When a host-executed task (or task group) lists one or more `#Service` nodes in
`dependsOn`:

1. cuenv starts the referenced services (and their transitive service
   dependencies) in an isolated ephemeral session directory.
2. cuenv waits for each service's configured readiness probe (`port`, `http`,
   `log`, `command`, or `delay`).
3. The task runs on the host with `cuenv-cas` skipped (because the task depends
   on live service state).
4. cuenv tears down all ephemeral services and removes the session directory
   when the task finishes or is cancelled, including cancellation during startup.

Host ephemeral services require host-native entrypoints. Use `cuenv up` to run
services with task or image prerequisites. Directory isolation still applies to
the task when live services disable its result cache.
Tasks in one cuenv process wait for a conflicting ephemeral service or host
port before starting. Services share the host network with other processes, so
allocate distinct ports for concurrent cuenv invocations.

## Generating a Dagger 1.0 Workspace (`cuenv sync ci` & `--export`)

Add `"dagger"` to `ci.providers` in `env.cue`:

```cue
ci: {
	providers: ["dagger"]
	provider: dagger: {
		defaultImage: "postgres:17-alpine"
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

Run the example in the cuenv checkout with
`cuenv task --path examples/dagger-basic --package examples integration`.
The example uses a disposable database inside a Dagger service and connects to it
by its service name.

Then sync or export:

```bash
# Emit dagger.toml, .dagger/modules/<name>/dagger-module.toml, and main.dang
cuenv sync ci --provider dagger

# Check for drift in CI
cuenv sync ci --provider dagger --check

# Print the Dagger module for the checked-in example to stdout
cuenv ci --export dagger --path examples/dagger-basic --package examples --pipeline default
```

Generated modules expose checks through `dagger check` and public services
through `dagger start` from a Git checkout. An expanded check runs its prerequisites and receives
their declared file outputs. Thin checks invoke the selected cuenv pipeline in
its project, using the module's CUE package and environment selection.

`workspaceDir` locates the workspace within the repository. `modulePath`
locates the generated module within that workspace and defaults to
`.dagger/modules/<moduleName>`. `engineVersion` defaults to `v1.0.0-beta.28`.
Thin checks download the matching cuenv Linux release. To use a pre-release
build or a bundled binary, set `ci.provider.dagger.cuenvBinary` to its path
within the workspace.

Use thin mode for cross-project dependencies, matrix or inline tasks,
contributors, dynamic environment resolution, and other features that need
cuenv’s pipeline scheduler. Expanded generation reports unsupported
configuration before writing the module.
