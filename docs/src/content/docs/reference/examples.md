---
title: Examples
description: A curated index of the real, tested cuenv configs under examples/, one canonical snippet per feature.
---

Every snippet on this page is excerpted from a config that lives under
[`examples/`](https://github.com/cuenv/cuenv/tree/main/examples) in the cuenv
repository. Those configs are evaluated in CI, so what you copy here is what the
tool actually runs.

Clone the repo and run any of them directly:

```bash
git clone https://github.com/cuenv/cuenv.git
cd cuenv

# Every example is loaded the same way: point --path at its directory
# and use the shared "examples" package.
cuenv env print --path examples/env-basic --package examples
cuenv task --path examples/task-basic --package examples
```

:::note[Status first]
This page links each feature to its example, but it does **not** decide what is
production-ready. Before you depend on a feature, check the
[Schema status](/reference/schema/status/) page and the
[schema coverage matrix](https://github.com/cuenv/cuenv/blob/main/docs/design/specs/schema-coverage-matrix.md).
Some definitions are `partial` or `schema-only`; this page calls those out where
they appear.
:::

## How an example is shaped

All examples share one header convention. They declare the package, import the
schema, anchor the file to `schema.#Project`, then set top-level fields like
`name` and `env`:

```cue
package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "env-basic"

env: {
	DATABASE_URL: "postgres://localhost/mydb"
}
```

`schema.#Project` on its own line constrains the whole file; you then fill in the
fields. (Some examples nest everything inside `schema.#Project & { ... }` — both
forms are valid, but the top-level form is what most examples use.)

:::caution[Environment values are strings]
Every value under `env:` must be a string. Write `PORT: "3000"`, not
`PORT: 3000`, and `DEBUG: "true"`, not `DEBUG: true`. cuenv exports environment
variables, and environment variables are strings. See
[Typed environments](/how-to/typed-environments/) for type-safe constraints like
`PORT: string & =~"^[0-9]+$"`.
:::

## Environments and secrets

[`examples/env-basic`](https://github.com/cuenv/cuenv/tree/main/examples/env-basic)
shows plain values plus CUE string interpolation.

```cue
package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "env-basic"

env: {
	DATABASE_URL: "postgres://localhost/mydb"
	DEBUG:        "true"
	PORT:         "3000"

	// Interpolation references another key
	BASE_URL:     "https://api.example.com"
	API_ENDPOINT: "\(BASE_URL)/v1"
}
```

```bash
cuenv env print --path examples/env-basic --package examples
cuenv env print --path examples/env-basic --package examples --output json
```

Secrets are resolved at runtime, never stored on disk. For a custom command-based
secret, use `schema.#ExecSecret` — not the `schema.#Secret & {command, args}`
shape:

```cue
env: {
	DB_PASSWORD: schema.#ExecSecret & {
		command: "vault"
		args: ["kv", "get", "-field=password", "secret/db"]
	}
}
```

Named resolvers exist too. `#OnePasswordRef`, `#AwsSecret`, and `#GcpSecret` are
implemented (feature-gated); `#VaultSecret` is schema-only until its runtime
resolver is registered. See the [secrets how-to](/how-to/secrets/) and confirm
support on the [status page](/reference/schema/status/).

## Tasks

[`examples/task-basic`](https://github.com/cuenv/cuenv/tree/main/examples/task-basic)
covers commands, scripts, sequences, and parallel groups in one file.

```cue
package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "task-basic"

env: {
	NAME: "Jack O'Neill"
}

tasks: {
	// A single command with arguments
	interpolate: schema.#Task & {
		command: "echo"
		args: ["Hello ", env.NAME, "!"]
	}

	// Steps run in order
	greetAll: schema.#TaskSequence & [
		schema.#Task & {command: "echo", args: ["Hello 1 ", env.NAME, "!"]},
		schema.#Task & {command: "echo", args: ["Hello 2 ", env.NAME, "!"]},
	]

	// Children run in parallel
	greetIndividual: schema.#TaskGroup & {
		type: "group"
		jack: schema.#Task & {command: "echo", args: ["Hello Jack"]}
		tealc: schema.#Task & {command: "echo", args: ["Hello Teal'c"]}
	}

	// A script with an explicit shell
	shellExample: schema.#Task & {
		script: """
			echo "Hello from Bash"
			"""
		scriptShell: "bash"
		shellOptions: errexit: true
	}
}
```

```bash
cuenv task --path examples/task-basic --package examples
cuenv task interpolate --path examples/task-basic --package examples
cuenv task greetIndividual.jack --path examples/task-basic --package examples
```

Groups, sequences, params, and caching are real. `timeout`, `retry`,
`continueOnError`, and group `maxConcurrency` carry limitations — see
[status](/reference/schema/status/) and [Run tasks](/how-to/run-tasks/).

## Task output references

[`examples/task-output-ref`](https://github.com/cuenv/cuenv/tree/main/examples/task-output-ref)
wires one task's `stdout` into another. Referencing the output **auto-infers the
dependency** — no manual `dependsOn` required for the reference.

```cue
package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "task-output-ref"

tasks: {
	tmpdir: schema.#Task & {
		command: "mktemp"
		args: ["-d"]
	}

	// tasks.tmpdir.stdout implies a dependency on tmpdir
	work: schema.#Task & {
		command: "echo"
		args: ["working in", tasks.tmpdir.stdout]
	}

	cleanup: schema.#Task & {
		command: "rm"
		args: ["-rf", tasks.tmpdir.stdout]
		dependsOn: [work]
	}
}
```

```bash
cuenv task work --path examples/task-output-ref --package examples
```

## Hooks

[`examples/hook`](https://github.com/cuenv/cuenv/tree/main/examples/hook) runs a
command on directory entry through the shell integration. Hooks require approval
before shell integration executes them; `cuenv exec` and `cuenv task` do not run
these hooks or require hook approval.

```cue
package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "hook"

env: {
	CUENV_TEST:   "loaded_successfully"
	API_ENDPOINT: "http://localhost:8080/api"
}

// onEnter hooks are keyed maps of named entries
hooks: onEnter: notify: {command: "echo", args: ["Environment configured"]}

tasks: {
	verify_env: schema.#Task & {
		command: "sh"
		args: ["-c", "echo CUENV_TEST=$CUENV_TEST"]
	}
}
```

```bash
# Approve the config and start shell hook execution
cuenv allow --path examples/hook
cuenv env load --path examples/hook --package examples

# Tasks use static env directly and do not require hook approval
cuenv task verify_env --path examples/hook --package examples
```

## CI pipeline

[`examples/ci-pipeline`](https://github.com/cuenv/cuenv/tree/main/examples/ci-pipeline)
declares a pipeline by referencing tasks. A `let` binds the tasks block so the
pipeline can point at concrete task values.

```cue
package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

let _t = tasks

name: "ci-pipeline"

ci: pipelines: {
	default: {
		tasks: [_t.test]
	}
}

tasks: {
	test: schema.#Task & {
		command: "echo"
		args: ["Running test task"]
		inputs: ["env.cue"]
	}
}
```

```bash
# Generate provider workflows (GitHub is the strongest path)
cuenv sync ci --path examples/ci-pipeline --package examples
```

GitHub CI sync is the most complete. Buildkite sync is partial; GitLab is
schema-only and `cuenv sync` rejects it. Emitting workflows also requires an
explicit `ci.providers` list. Confirm on the [status page](/reference/schema/status/).

## Services

[`examples/services-readiness`](https://github.com/cuenv/cuenv/tree/main/examples/services-readiness)
demonstrates every readiness `kind`, plus `restart` and `watch`.

```cue
package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "services-readiness"

services: {
	// Wait for a TCP port to accept connections
	port: schema.#Service & {
		dependsOn: [tasks.prepare]
		entrypoint: schema.#Command & {command: "python3", args: ["-c", "..."]}
		readiness: {kind: "port", port: 18080}
		shutdown: timeout: "2s"
	}

	// Wait for an HTTP endpoint
	http: schema.#Service & {
		entrypoint: schema.#Command & {command: "python3", args: ["-m", "http.server", "18081"]}
		readiness: {kind: "http", url: "http://127.0.0.1:18081/"}
		shutdown: timeout: "2s"
	}

	// Wait for a log line; restart on file change
	delay: schema.#Service & {
		entrypoint: schema.#Command & {command: "sh", args: ["-c", "while :; do sleep 60; done"]}
		readiness: {kind: "delay", delay: "1s"}
		restart: {mode: "unlessStopped", maxRestarts: 3, window: "30s"}
		watch: {paths: ["env.cue"], on: "restart"}
		shutdown: timeout: "2s"
	}
}
```

Readiness kinds in the example: `port`, `http`, `log`, `command`, and `delay`.

```bash
cuenv up --path examples/services-readiness --package examples
cuenv ps --path examples/services-readiness --package examples
cuenv down --path examples/services-readiness --package examples
```

Service-to-service dependencies and task `dependsOn` are honored. Image
dependencies are recognized but image execution backends are still being wired
in. See [status](/reference/schema/status/).

## Container images

[`examples/container-image`](https://github.com/cuenv/cuenv/tree/main/examples/container-image)
shows Dockerfile builds and a Nix-native build from a flake output.

```cue
package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "container-image-example"

images: {
	// Dockerfile build with task and input dependencies
	api: schema.#ContainerImage & {
		context:    "."
		dockerfile: "Dockerfile"
		tags: ["latest", "v1.0.0"]
		dependsOn: [tasks.codegen]
		inputs: ["src/**", "Dockerfile"]
	}

	// Nix-native: built from a flake output, no Dockerfile
	tools: schema.#ContainerImage & {
		installable: ".#images.tools"
		tags: ["latest"]
		registry:   "ghcr.io/myorg"
		repository: "myorg/tools"
	}
}
```

```bash
cuenv build --path examples/container-image --package examples
```

`#ContainerImage` is **partial**: `cuenv build` lists and builds images with the
local Docker CLI, and registry builds push via buildx. Dagger execution and
downstream image output-reference resolution are incomplete — see
[status](/reference/schema/status/).

## Codegen

[`examples/codegen-hello`](https://github.com/cuenv/cuenv/tree/main/examples/codegen-hello)
generates files from CUE using typed file kinds and a shared `context`.

```cue
package examples

import (
	"github.com/cuenv/cuenv/schema"
	gen "github.com/cuenv/cuenv/schema/codegen"
)

schema.#Project & {
	name: "codegen-hello-example"

	codegen: {
		context: serviceName: "hello-world"

		files: {
			"package.json": gen.#JSONFile & {
				mode: "managed"
				content: """
					{ "name": "\(context.serviceName)", "version": "1.0.0" }
					"""
			}
			"src/main.ts": gen.#TypeScriptFile & {
				mode: "scaffold"
				content: """
					console.log("Hello, \(context.serviceName)!");
					"""
			}
		}
	}
}
```

`mode: "managed"` keeps the file in sync; `mode: "scaffold"` writes it once and
leaves it alone.

## CODEOWNERS and rules

[`examples/owners-basic`](https://github.com/cuenv/cuenv/tree/main/examples/owners-basic)
uses the **legacy top-level `owners:`** shape.

```cue
package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "owners-basic"

owners: {
	output: platform: "github"
	rules: {
		"default": {pattern: "*", owners: ["@core-team"], order: 0}
		"rust-files": {pattern: "*.rs", owners: ["@rust-team"], section: "Backend", order: 1}
		"docs": {pattern: "/docs/**", owners: ["@docs-team"], order: 3}
	}
}
```

:::caution[Prefer the .rules.cue path]
This is the legacy shape kept for compatibility. For new projects, define
ownership through the canonical `.rules.cue` schemas and the default sync
provider rather than top-level `owners:`. Do **not** use `cuenv sync codeowners`.
See the [CODEOWNERS how-to](/how-to/codeowners/).
:::

## VCS dependencies

[`examples/vcs-subdir`](https://github.com/cuenv/cuenv/tree/main/examples/vcs-subdir)
pulls a subdirectory of another git repo into the project.

```cue
package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
	name: "vcs-subdir"

	vcs: "agent-skills": {
		url:       "https://github.com/cuenv/cuenv.git"
		reference: "main"
		vendor:    false
		subdir:    ".agents/skills"
		path:      ".agents/skills"
	}

	tasks: inspect: schema.#Task & {
		command: "sh"
		args: ["-c", "find .agents/skills -maxdepth 2 -type f | sort | head"]
	}
}
```

```bash
cuenv task inspect --path examples/vcs-subdir --package examples
```

## Tools

[`examples/ci-bun-workspace`](https://github.com/cuenv/cuenv/tree/main/examples/ci-bun-workspace)
pins a tool through a contrib module and runs it.

```cue
package examples

import (
	"github.com/cuenv/cuenv/schema"
	xBun "github.com/cuenv/cuenv/contrib/bun"
)

schema.#Project

name: "ci-bun-workspace"

runtime: schema.#ToolsRuntime & {
	platforms: ["linux-x86_64", "darwin-arm64"]
	tools: {
		bun: xBun.#Bun & {version: "1.1.0"}
	}
}

tasks: {
	version: schema.#Task & {
		command: "bun"
		args: ["--version"]
	}
}
```

The tool map accepts either a bare version string or a full `#Tool`. A bare
string (`jq: "1.7.1"`) is **only valid when a source is configured** — cuenv has
no implicit default source. There is no Homebrew tool source: `#Source` is
`#Oci | #GitHub | #Nix | #Rustup | #URL`. So spell the source out:

```cue
runtime: schema.#ToolsRuntime & {
	platforms: ["darwin-arm64", "linux-x86_64"]
	tools: {
		// Explicit GitHub Releases source
		gh: {
			version: "2.62.0"
			source: schema.#GitHub & {
				repo:  "cli/cli"
				tag:   "v{version}"
				asset: "gh_{version}_{os}_{arch}.tar.gz"
				path:  "gh_{version}_{os}_{arch}/bin/gh"
			}
		}

		// Or a contrib module that fills in the source for you
		bun: xBun.#Bun & {version: "1.1.0"}
	}
}
```

```bash
cuenv sync lock --path examples/ci-bun-workspace --package examples
cuenv exec --path examples/ci-bun-workspace --package examples -- bun --version
```

See [Tools](/how-to/tools/) for sources, overrides, and shell activation.

## Installing cuenv

This page does not duplicate install guidance. The
[installation how-to](/how-to/install/) is the canonical source. Note that
cuenv is not currently published to crates.io, so `cargo install cuenv` will not
work — use a release binary, Nix, Homebrew, or `cargo install --path
crates/cuenv` from a clone.

## See also

- [Schema status](/reference/schema/status/) — what is stable, partial, or schema-only
- [Configure a project](/how-to/configure-a-project/)
- [Run tasks](/how-to/run-tasks/)
- [Typed environments](/how-to/typed-environments/)
- [Secrets](/how-to/secrets/)
- [Tools](/how-to/tools/)
- [CLI reference](/reference/cli/)
