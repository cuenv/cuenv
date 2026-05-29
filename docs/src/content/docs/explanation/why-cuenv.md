---
title: Why cuenv
description: The rationale behind cuenv — one typed CUE contract for environment, tasks, secrets, and CI, validated before it runs, with an honest map of what is stable today.
---

import { Aside } from '@astrojs/starlight/components';

Most projects don't have a configuration system. They have a pile.

A `.env` file holds the variables. A `Makefile` or `justfile` holds the tasks.
A hand-written CI workflow tries to reproduce both in YAML. Secrets live in a
fourth place — a password manager, a cloud secret store, or worst of all,
committed by accident. Nothing validates any of it, and the pieces drift apart
the moment someone changes one without the others.

cuenv replaces the pile with a single typed contract. This page explains why
that contract exists, why it's written in [CUE](https://cuelang.org), and why
the same definitions drive both your laptop and your CI. It also tells you,
honestly, which parts of that vision are finished today and which are still
landing — because inspiration that can't be trusted isn't worth much.

## The problem: configuration sprawl

Every layer of a typical project's configuration is untyped and disconnected:

| What you have | What goes wrong |
| --- | --- |
| `.env` — flat strings | `NODE_ENV=prodction` is valid text; nothing catches it until production breaks |
| `Makefile` / `justfile` — shell recipes | Task dependencies are implicit; parallelism is manual; a typo'd target fails at runtime |
| CI YAML — a second copy of your tasks | Hand-maintained to match the Makefile, and it always falls behind |
| Secret stores — env, 1Password, Vault, cloud | Referenced by convention, easy to forget, easy to leak into logs or commits |

None of these layers know about each other. The `.env` doesn't know the CI
needs `DATABASE_URL`. The CI doesn't know the Makefile renamed `build` to
`compile`. There is no single place that says "this is what a valid version of
this project looks like" — and so there is no single place to validate.

## The cuenv answer: one closed, typed contract

cuenv collapses that pile into a single `env.cue` that conforms to one schema
definition: [`#Project`](https://github.com/cuenv/cuenv/blob/main/schema/core.cue).
`#Project` is a **closed** struct — CUE rejects any field it doesn't recognise —
so the contract is exhaustive and typo-proof by construction. It carries every
concern in one shape:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
	name: "checkout-api"

	env: {
		// Only these values are accepted; defaults to "development".
		NODE_ENV: "development" | "staging" | "production" | *"development"
		PORT:     "8080"

		// Resolved at runtime from 1Password. Never written to disk.
		DATABASE_PASSWORD: schema.#OnePasswordRef & {
			ref: "op://Engineering/checkout-db/password"
		}
	}

	tasks: {
		// Object keys run in parallel.
		check: schema.#TaskGroup & {
			type: "group"
			lint: schema.#Task & {command: "npm", args: ["run", "lint"]}
			test: schema.#Task & {command: "npm", args: ["test"]}
		}

		// Waits for `check`; only re-runs when its inputs change.
		build: schema.#Task & {
			command:   "npm"
			args:      ["run", "build"]
			dependsOn: [check]
			inputs:    ["src/**", "package.json"]
			outputs:   ["dist/**"]
			cache: mode: "read-write"
		}
	}
}
```

Where the old pile had four files and zero validation, this has one file and a
schema that fails loudly the instant a value doesn't fit. The `before` is
`.env` + `Makefile` + CI YAML + a secret-store convention, each maintained
separately and silently drifting. The `after` is a single document the tooling
can read, type-check, and act on:

```bash
cuenv task            # list what the project defines
cuenv env print       # print the resolved environment (secrets redacted)
cuenv task build      # run `check` in parallel, then `build`
cuenv exec -- npm start   # run any command in the validated environment
```

The `#Project` schema reaches well beyond this example. It also carries
`hooks` (shell and directory activation), `ci` (pipeline definitions),
`services` (long-running processes), `images` (container builds), `codegen`,
`runtime` (tool provisioning), `vcs` (sparse dependencies), and `release`.
Not all of those are equally finished — see [Honest scope](#honest-scope-what-is-and-isnt-finished)
below — but they all live in the same closed contract, which is the point.

## Why CUE specifically

You could imagine doing this with YAML and a JSON-schema validator bolted on.
cuenv uses CUE instead, deliberately. CUE is not a templating language with
types stapled to it; types, values, and constraints are the same thing.

- **Types and constraints unify.** `NODE_ENV: "development" | "staging" | "production"`
  is both the type and the allowed set of values. `PORT: string & =~"^[0-9]+$"`
  constrains a value with a regex. You don't validate against a separate schema
  file — the constraint *is* the value's definition. A bad value isn't a lint
  warning; it's an evaluation error.
- **Composition replaces templating.** CUE merges configurations by unification,
  not string substitution. A base project and a per-environment override combine
  into one consistent value, and CUE rejects the result if they conflict. There
  is no `{{ .Values.foo | default "bar" }}` guesswork and no way for two
  templates to silently produce contradictory output.
- **The schema is the source of truth.** cuenv's own
  [`schema/*.cue`](https://github.com/cuenv/cuenv/tree/main/schema) files define
  what a valid project is. Your `env.cue` unifies against them, so the editor,
  the CLI, and CI all agree on the same contract. The
  [schema status page](/reference/schema/status/) tracks exactly which parts of
  that contract are wired through to behaviour.
- **Validation happens before execution.** CUE evaluation runs first. If the
  contract doesn't hold — a missing required field, a value outside an enum, a
  `dependsOn` reference that doesn't resolve — cuenv stops before it runs a
  single command. The cheapest bug is the one that never executes.

<Aside type="tip" title="Validate before you run">
`cuenv env print` evaluates the whole project and prints the resolved
environment with secrets redacted. If the CUE is invalid, you find out here —
not three steps into a deployment.
</Aside>

See [Typed environments](/how-to/typed-environments/) for the practical patterns
and [Secrets](/how-to/secrets/) for how runtime secret references stay out of
files.

## Why the unified DAG

The same `tasks` block that you run on your laptop is the source of your CI.
cuenv builds one task graph — a DAG resolved by CUE references, not string
names — and runs it the same way everywhere.

This matters for three reasons:

- **No second copy of your pipeline.** `cuenv sync ci` generates a GitHub
  Actions workflow from the same task definitions you run locally, so the CI
  can't drift away from `make build`. Running `cuenv ci` executes those
  pipelines locally, against the identical graph. (See
  [CI](/how-to/ci/).)
- **Hermetic execution.** Each task runs in a fresh working directory populated
  only from its declared `inputs`. A task can't accidentally depend on a file it
  didn't list, which is exactly the class of bug that makes "works on my
  machine" possible. This directory-isolation model is described in
  [the hermetic-cache ADR](/decisions/adrs/adr-task-hermetic-cache/).
- **Content-addressed caching.** A task's cache key is a SHA-256 over its
  resolved input file hashes, command, arguments, resolved environment, and the
  cuenv version and platform. If nothing in that envelope changed, the task is
  skipped. The cache is keyed by *content*, so it's correct to share across runs
  and machines.

Dependencies are CUE references — `dependsOn: [check]` points at the actual
`check` value, not the string `"check"`. A typo is an evaluation error, not a
silent no-op at runtime. The execution semantics behind `cuenv task` —
discovery, graph building, parallel groups versus ordered sequences, and
first-failure abort — are recorded in
[RFC-0004](/decisions/rfcs/rfc-0004-task-execution-ux-and-dependency-strategy/).

Typed environments, `cuenv exec`, task groups/sequences/dependencies/params,
content-addressed caching, and GitHub Actions generation are all **Stable**
today. See [Run tasks](/how-to/run-tasks/) to put the DAG to work.

## Honest scope: what is and isn't finished

cuenv's contract is ambitious, and not all of it is equally finished. The
[schema status page](/reference/schema/status/) and the
[schema coverage matrix](https://github.com/cuenv/cuenv/blob/main/docs/design/specs/schema-coverage-matrix.md)
are the authoritative sources. Status falls into three buckets: **Stable**
(safe to rely on), **Partial** (works within documented limits), and
**Preview / schema-only** (defined in the schema but not usable yet).

So the inspiration above stays trustworthy, here is what is *not* fully done:

- **Services** (`cuenv up`/`ps`/`down`/`logs`/`restart`) — **Partial.** Sessions,
  readiness, restart, watch, and service-plus-task dependencies work. Services
  that depend on image builds still fail fast until image execution backends are
  wired into service startup.
- **Container images** (`cuenv build`) — **Partial.** Builds Dockerfile images
  via the local Docker CLI / buildx and Nix images. Multi-arch Nix, Dagger
  execution, and downstream image output-reference resolution are incomplete.
- **OCI and Dagger runtimes** — **Partial.** `cuenv sync lock` resolves OCI
  digests and `extract` paths into `cuenv.lock`, and the OCI activation hook
  prepends extracted binaries to `PATH`. The **container runtime is Preview**
  and not complete.
- **Release automation** (`cuenv release`) — **Partial.** Version, publish, and
  binary flows exist; config-driven release backends are incomplete.
- **CI beyond GitHub** — GitHub Actions sync is **Stable** and the strongest
  path. **Buildkite** export is **Partial**. **GitLab** is **schema-only** —
  `cuenv sync ci` rejects it with a configuration error until an emitter exists.
- **Secrets: Vault** — **Preview.** `#VaultSecret` is in the schema, but no
  runtime resolver is registered yet. Environment variables, `#ExecSecret`,
  1Password, Infisical, AWS Secrets Manager, and GCP Secret Manager all resolve
  today and are redacted from output.
- **Task execution policies** — `timeout`, `retry`, `continueOnError`, and group
  `maxConcurrency` are schema-visible but **not fully enforced** yet.

If you're evaluating cuenv, lean on the **Stable** core — typed environments,
`exec`, the task DAG with caching, shell hooks, and GitHub CI generation — and
treat the **Partial** and **Preview** surfaces as direction, not promises.

## Where to go next

- New to cuenv? Start with [Your first cuenv project](/tutorials/first-project/).
- Want the landing-page overview? See the [home page](/).
- Ready to build the DAG? [Run tasks](/how-to/run-tasks/) and
  [keep secrets out of files](/how-to/secrets/).
- Generating CI from the same definitions? [CI](/how-to/ci/).
- Curious how it's built? Read the [architecture overview](/explanation/architecture/)
  and the [decisions log](/decisions/).
