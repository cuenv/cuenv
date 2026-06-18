---
title: CI
description: Generate GitHub Actions workflows from your task graph, run the same DAG locally with cuenv ci, and export pipelines for other providers — honestly scoped to what each provider supports today.
---

import { Aside, Card, CardGrid, Tabs, TabItem } from "@astrojs/starlight/components";

You already wrote your build, lint, and test steps once — as cuenv tasks. CI
should not make you write them again as YAML, with their own copy of the
dependency order, their own cache keys, and their own subtle drift from what runs
on your laptop. With cuenv it doesn't: `cuenv sync ci` generates the workflow
files from the same task graph, and `cuenv ci` runs that exact graph locally. One
typed config, the same DAG everywhere, no hand-maintained YAML to fall behind.

<Aside type="note" title="Provider status">
GitHub Actions sync is the **stable**, strongest path. Buildkite sync/export is
**partial** (export only). GitLab export/sync is **schema-only** — there is no
emitter, and `cuenv sync ci --provider gitlab` (and `--export gitlab`) is
rejected with a configuration error. CircleCI has an export flag. See
[Schema status](/reference/schema/status/) before relying on a non-GitHub path.
</Aside>

## What you get

You write a pipeline that points at task references. Here is the minimal shape,
derived from [`examples/ci-pipeline`](https://github.com/cuenv/cuenv/tree/main/examples/ci-pipeline):

```cue
package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

let _t = tasks

name: "ci-pipeline"

ci: {
	// Required: explicit opt-in. No file is emitted without a provider.
	providers: ["github"]

	pipelines: {
		default: {
			tasks: [_t.test]
		}
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

Run `cuenv sync ci` and cuenv writes the GitHub Actions workflow for you. An
excerpt of the generated `.github/workflows/*.yml` reveals the payoff — the
scaffolding you never typed:

```yaml
# .github/workflows/<pipeline>.yml (generated — do not edit by hand)
name: default
on:
  push: {}
jobs:
  cuenv:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - name: Setup cuenv
        run: # download + cuenv sync ci, injected by the cuenv contributor
      - name: Run pipeline
        run: cuenv ci --pipeline default
```

The exact YAML depends on your contributors (Nix install, Cachix, 1Password, and
so on are spliced in automatically) and the generation mode below. The point is
that the dependency order, the setup steps, and the cache keys all come from your
CUE — not from a YAML file you have to keep in sync by hand. Commit the generated
files and let CI check them with `cuenv sync ci --check`.

Pipeline tasks are CUE **references**, not string names. Binding `let _t = tasks`
and referencing `_t.<task>` keeps task refs type-checked and lets cuenv derive
dependencies from the same graph `cuenv task` uses.

## Generate workflows

```bash
# Write the GitHub Actions workflow files.
cuenv sync ci

# Verify committed workflows match the CUE (fails CI on drift).
cuenv sync ci --check

# Preview the diff without writing.
cuenv sync ci --dry-run

# Sync every project in a workspace.
cuenv sync ci -A
```

`cuenv sync ci` accepts `--provider <github|buildkite>` to restrict which
provider emits. `--provider gitlab` is rejected with a configuration error until
a GitLab emitter exists.

## Thin vs expanded mode

Each pipeline has a generation `mode` (schema [`#PipelineMode`](https://github.com/cuenv/cuenv/blob/main/schema/ci.cue)),
which controls how much of your DAG the generated workflow inlines:

| Mode | Default | What the workflow contains | Use when |
| --- | --- | --- | --- |
| `thin` | yes | A minimal workflow: bootstrap contributors → one `cuenv ci --pipeline <name>` orchestration job → finalizer contributors. cuenv runs your DAG inside that job. | You want CI to mirror local runs exactly, with cuenv owning scheduling, parallelism, and caching. The simplest, most drift-resistant choice. |
| `expanded` | no | Every task expanded as its own provider job/step with native dependencies between them. | You want each task to surface as a separate CI job (per-task status, native retries, provider-level parallelism, or matrix fan-out at the provider). |

```cue
ci: pipelines: {
	ci: {
		mode: "expanded" // default is "thin"
		tasks: [_t.lint, _t.test]
	}
}
```

Thin mode keeps the workflow tiny and pushes all orchestration into `cuenv ci`,
so what you see locally is what runs in CI. Expanded mode trades that for native
provider visibility per task.

## Affected-task detection

`cuenv ci` runs the **same DAG locally** that CI runs — there is no separate code
path. On a pull request it can skip tasks whose inputs did not change, using
`--from <ref>` to pick the base to compare against:

```bash
# Only run tasks whose inputs changed versus origin/main.
cuenv ci --from origin/main
```

Detection is **inputs-driven**: cuenv compares the files matched by each task's
`inputs` globs against the diff from the base ref. A task with **no `inputs`
always runs** (cuenv cannot prove it is unaffected). If `--from` is omitted,
cuenv uses the provider's default base (for example, a PR's base branch). Declare
tight `inputs` on every task to get the most out of affected detection.

See the contributor-driven setup that wraps these runs in
[CI Contributors](/reference/ci-contributors/), which injects the Nix/cuenv/cache
bootstrap around the affected DAG.

## Matrix builds and artifacts

A pipeline task can be a plain task reference **or** a matrix task
([`#MatrixTask`](https://github.com/cuenv/cuenv/blob/main/schema/ci.cue)). A
matrix task fans one task out across dimensions and can download artifacts from
upstream tasks first:

```cue
ci: pipelines: {
	release: {
		tasks: [
			{
				type: "matrix"
				task: _t.build
				// Dimensions: each key expands across its values.
				matrix: {
					arch: ["linux-x64", "darwin-arm64"]
				}
				// Parameters passed to the task per variant.
				params: {
					profile: "release"
				}
			},
			{
				type: "matrix"
				task: _t.bundle
				matrix: os: ["linux", "macos"]
				// Pull artifacts produced by an upstream task before running.
				artifacts: [{
					from:   "build"          // source task (must declare outputs)
					to:     "dist"           // directory to download into
					filter: "*stable"        // optional glob over matrix variants
				}]
			},
		]
	}
}
```

- `matrix` is a map of dimension name → values; the task runs once per
  combination.
- `params` passes string parameters to each variant of the task.
- `artifacts[]` is a list of [`#ArtifactDownload`](https://github.com/cuenv/cuenv/blob/main/schema/ci.cue):
  `from` is the source task (which must have outputs), `to` is the target
  directory, and the optional `filter` glob selects which matrix variants to pull.

Provider-native matrix workflows are produced by `cuenv sync ci`. Local
`cuenv ci` does not yet filter matrices (see the reserved flag below).

## Export and run flags

`cuenv ci` runs the pipeline by default and can also export it for another
provider. The full flag set (verified against
[`crates/cuenv/src/commands/ci/args.rs`](https://github.com/cuenv/cuenv/blob/main/crates/cuenv/src/commands/ci/args.rs)):

| Flag | Purpose |
| --- | --- |
| `-p, --pipeline <NAME>` | Pipeline to run or export (defaults to `default`). |
| `--export <FORMAT>` | Export pipeline YAML to stdout instead of running. `FORMAT` is `buildkite`, `gitlab`, `github-actions`, or `circleci`. |
| `-o, --output <PATH>` | Write exported YAML to a file instead of stdout. |
| `--from <REF>` | Base ref for affected-task detection. |
| `-j, --jobs <N>` | Max parallel DAG jobs (`0` = host parallelism, the default). |
| `--dry-run` | Show what would run without executing. |
| `-e, --environment <NAME>` | Environment used for secret resolution. |
| `--path <PATH>` / `--package <PACKAGE>` | CUE directory and package (default `.` / `cuenv`). |

```bash
# Run the default pipeline locally.
cuenv ci

# Run a named pipeline, capped at 4 parallel jobs.
cuenv ci --pipeline ci --jobs 4

# See the plan without executing.
cuenv ci --pipeline ci --dry-run

# Export a dynamic Buildkite pipeline and upload it.
cuenv ci --pipeline ci --export buildkite | buildkite-agent pipeline upload

# Export a GitHub Actions workflow to a file.
cuenv ci --pipeline ci --export github-actions --output .github/workflows/ci.yml
```

<Aside type="caution" title="`--filter-matrix` is reserved">
`--filter-matrix` exists in the CLI but is **reserved and rejected** — local
matrix filtering is not supported yet. Use `cuenv sync ci` to generate
provider-native matrix workflows.
</Aside>

## Provider support, honestly

Export formats and sync paths differ in maturity. Mirror this when you choose a
provider; do not treat the non-GitHub paths as production-ready.

| Provider | `cuenv sync ci` | `cuenv ci --export` | Status |
| --- | --- | --- | --- |
| GitHub Actions | Yes — full emitter | `github-actions` | **Stable** |
| Buildkite | Partial | `buildkite` | **Partial** (export-focused) |
| GitLab | Rejected (no emitter) | `gitlab` accepted by the flag, but no emitter | **Schema-only** |
| CircleCI | — | `circleci` flag present | Export flag only |

This reconciles with the [CLI reference](/reference/cli/) note on `cuenv ci` and
the [schema status](/reference/schema/status/) page: GitHub CI sync is the
strongest path; Buildkite sync/export is partial; GitLab export/sync is
schema-only and sync rejects it with a configuration error until an emitter
exists.

## Provider configuration

Provider-specific settings live under `ci.provider.<name>` (schema
[`#ProviderConfig`](https://github.com/cuenv/cuenv/blob/main/schema/ci.cue)).
GitHub is the most complete:

```cue
ci: {
	providers: ["github"]

	provider: github: {
		runner: "ubuntu-latest"
		permissions: {
			contents: "read"
		}
	}
}
```

Per-pipeline `providers` **completely replaces** the global list for that
pipeline (no merge):

```cue
ci: {
	providers: ["github"]

	pipelines: {
		release: {
			providers: ["buildkite"] // overrides the global ["github"]
			tasks: [_t.release]
		}
	}
}
```

## Secrets in CI

Use task-local secret refs when cuenv should resolve the value at runtime:

```cue
tasks: {
	deploy: schema.#Task & {
		command: "bash"
		args: ["scripts/deploy.sh"]
		inputs: ["scripts/deploy.sh"]
		env: {
			DEPLOY_TOKEN: schema.#OnePasswordRef & {
				ref: "op://Production/Deploy/token"
			}
		}
	}
}
```

Use `schema.#EnvPassthrough` when the CI provider already exposes the value:

```cue
env: {
	GH_TOKEN: schema.#EnvPassthrough & {name: "GITHUB_TOKEN"}
}
```

See [How to manage secrets](/how-to/secrets/) for provider details.

## Pipeline annotations

A pipeline can declare **annotations** — named string values that appear in the
GitHub job summary table after the pipeline finishes. Annotations can be literal
strings or resolved from task captures.

### Literal annotations

```cue
ci: pipelines: default: {
    tasks: [tasks.deploy]
    annotations: {
        "Deployed to": "production"
        "Region":      "us-east-1"
    }
}
```

### Capture-backed annotations

A `#TaskCaptureRef` names a task and one of its named captures; the executor
resolves the value after the task runs. This is how deploy preview URLs, build
versions, or bundle sizes surface in CI summaries without piping between jobs.

First, define the capture on the task — the **first capture group** of the regex
becomes the named value:

```cue
tasks: {
    build: schema.#Task & {
        command: "my-build-tool"
        captures: {
            version: { pattern: "Built version ([^ ]+)" }
            size:    { pattern: "Bundle size: ([0-9.]+[kKmMgG]?B)", source: "stderr" }
        }
    }
}
```

Then reference those captures in the pipeline's `annotations` map:

```cue
ci: pipelines: default: {
    tasks: [tasks.build]
    annotations: {
        "Build version": schema.#TaskCaptureRef & {
            cuenvTask:    "build"
            cuenvCapture: "version"
        }
        "Bundle size": schema.#TaskCaptureRef & {
            cuenvTask:    "build"
            cuenvCapture: "size"
        }
    }
}
```

Annotations whose capture is not matched (e.g., the task didn't produce the
expected output) are silently dropped — the job summary omits them. See the
[runnable example](https://github.com/cuenv/cuenv/tree/main/examples/task-captures)
for a complete working project.

## Worked examples

Every scenario below maps to a runnable project under
[`examples/`](https://github.com/cuenv/cuenv/tree/main/examples). The setup steps
(Nix, cuenv, caches, secrets) are injected by
[contributors](/reference/ci-contributors/) — you write only the pipeline and
provider config shown.

<Tabs>
<TabItem label="Basic">

[`examples/ci-pipeline`](https://github.com/cuenv/cuenv/tree/main/examples/ci-pipeline)
— the minimal pipeline pointing at one task reference.

```cue
ci: pipelines: {
	default: {
		tasks: [_t.test]
	}
}
```

</TabItem>
<TabItem label="Cachix">

[`examples/ci-cachix`](https://github.com/cuenv/cuenv/tree/main/examples/ci-cachix)
— a Nix runtime plus a Cachix binary cache. Configuring `cachix` activates the
`#Cachix` contributor.

```cue
runtime: schema.#NixRuntime & {
	flake:  "."
	output: "devShells.x86_64-linux.default"
}

ci: {
	provider: github: cachix: {
		name: "my-project-cache"
	}
	pipelines: build: {
		tasks: [_t.build]
		when: branch: "main"
	}
}
```

</TabItem>
<TabItem label="1Password">

[`examples/ci-onepassword`](https://github.com/cuenv/cuenv/tree/main/examples/ci-onepassword)
— `op://` refs in an environment activate the 1Password contributor.

```cue
env: environment: production: {
	API_TOKEN:  schema.#OnePasswordRef & {ref: "op://vault/api/token"}
	DEPLOY_KEY: schema.#OnePasswordRef & {ref: "op://vault/deploy/key"}
}

ci: pipelines: deploy: {
	environment: "production"
	tasks: [_t.deploy]
	when: branch: "main"
}
```

</TabItem>
<TabItem label="gh models">

[`examples/ci-gh-models`](https://github.com/cuenv/cuenv/tree/main/examples/ci-gh-models)
— a `gh models` task activates the GitHub Models CLI contributor.

```cue
tasks: evalPrompts: schema.#Task & {
	command: "gh"
	args: ["models", "eval", "prompts/test.yml"]
	inputs: ["prompts/**/*.yml"]
}
```

</TabItem>
<TabItem label="Namespace cache">

[`examples/ci-namespace-cache`](https://github.com/cuenv/cuenv/tree/main/examples/ci-namespace-cache)
— Namespace nscloud Nix cache instead of Cachix (Linux runners).

```cue
import c "github.com/cuenv/cuenv/contrib/contributors"

ci: {
	contributors: [c.#NamespaceCache]
	provider: github: namespaceCache: {}
	pipelines: build: {
		tasks: [_t.build]
		when: branch: "main"
	}
}
```

</TabItem>
<TabItem label="cuenv install">

[`examples/ci-cuenv-homebrew`](https://github.com/cuenv/cuenv/tree/main/examples/ci-cuenv-homebrew)
and [`examples/ci-cuenv-nix`](https://github.com/cuenv/cuenv/tree/main/examples/ci-cuenv-nix)
— pick how CI installs cuenv via `config.ci.cuenv.source`.

```cue
// GitHub Release (default): pin to the cuenv version that generated the workflow.
config: ci: cuenv: {
	source: "release"
	// version defaults to "self"; use "latest" or "0.19.0" to override.
	// Generated downloads fail fast if the selected release asset is missing.
}

// Homebrew (no Nix required):
config: ci: cuenv: {
	source: "homebrew"
}

// Or build cuenv from the checked-out repository flake:
config: ci: cuenv: {
	source:  "nix"
	version: "self"
}
```

</TabItem>
<TabItem label="codecov">

[`examples/ci-codecov`](https://github.com/cuenv/cuenv/tree/main/examples/ci-codecov)
— the `#Codecov` contributor uploads coverage after a test task.

```cue
import xCodecov "github.com/cuenv/cuenv/contrib/codecov"

ci: {
	contributors: [xCodecov.#Codecov]
	pipelines: test: {
		tasks: [_t.test]
		when: pullRequest: true
	}
}
```

</TabItem>
<TabItem label="bun workspace">

[`examples/ci-bun-workspace`](https://github.com/cuenv/cuenv/tree/main/examples/ci-bun-workspace)
— the `#BunWorkspace` contributor injects `bun install` before `bun` tasks.

```cue
import (
	xBun "github.com/cuenv/cuenv/contrib/bun"
	c "github.com/cuenv/cuenv/contrib/contributors"
)

runtime: schema.#ToolsRuntime & {
	platforms: ["linux-x86_64", "darwin-arm64"]
	tools: bun: xBun.#Bun & {version: "1.1.0"}
}

ci: {
	contributors: [c.#Cuenv, c.#BunWorkspace]
	pipelines: default: {
		tasks: [_t.version]
		when: branch: "main"
	}
}
```

</TabItem>
</Tabs>

## See also

<CardGrid>
	<Card title="CI contributors" icon="puzzle">
		How Nix, cuenv, caches, and secret setup get injected into the DAG:
		[CI Contributors](/reference/ci-contributors/).
	</Card>
	<Card title="Run tasks" icon="rocket">
		Define the task graph CI runs: [Run tasks](/how-to/run-tasks/).
	</Card>
	<Card title="Secrets" icon="seti:lock">
		Runtime secret resolution: [How to manage secrets](/how-to/secrets/).
	</Card>
	<Card title="Schema status" icon="approve-check">
		Current CI provider coverage: [Schema status](/reference/schema/status/).
	</Card>
</CardGrid>
