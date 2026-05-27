# cuenv

One typed config for your whole project. Define your environment variables,
secrets, tasks, and CI in [CUE](https://cuelang.org); cuenv validates them,
resolves secrets at runtime, runs your tasks, and generates your pipelines.

[![License: AGPL v3](https://img.shields.io/badge/License-AGPL%20v3-blue.svg)](https://www.gnu.org/licenses/agpl-3.0)
[![Build Status](https://github.com/cuenv/cuenv/actions/workflows/cuenv-ci.yml/badge.svg)](https://github.com/cuenv/cuenv/actions/workflows/cuenv-ci.yml)
[![Crates.io](https://img.shields.io/crates/v/cuenv)](https://crates.io/crates/cuenv)

> [!WARNING]
> **Rapid iteration in progress.** I'm actively exploring the right APIs and
> schema to handle everything cuenv needs to do. Expect breaking changes between
> releases during this period. If you're using cuenv, be prepared for things to
> break.

## What is cuenv?

Most projects accumulate a pile of loosely related config. A `.env` file (or an
`.envrc`) for variables. A `Makefile` or `justfile` for tasks. A hand-written
`.github/workflows/*.yml` that tries to stay in sync with both. Nothing
validates any of it, secrets end up committed in `.env` files, and the three
slowly drift apart.

cuenv replaces that pile with a single `env.cue`. You describe your project once
in CUE — a typed configuration language — and cuenv handles the rest:

- **Validates** every value against a schema before anything runs.
- **Resolves secrets at runtime** from your provider, without writing them to disk.
- **Runs your tasks** with dependency ordering, parallelism, and optional caching.
- **Generates CI** workflows from the same task definitions.

It's written in Rust with a Go bridge to the CUE evaluator. The project moves
fast (see the warning above), but the environment, task, and GitHub CI paths are
in daily use.

## A complete example

Here is a small project — a typed environment with a runtime secret, plus a few
tasks. Save it as `env.cue`:

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
	name: "checkout-api"
}

env: {
	// Only these three values are accepted; defaults to "development".
	NODE_ENV: "development" | "staging" | "production" | *"development"

	// Ordinary values, with CUE interpolation.
	HOST: "127.0.0.1"
	PORT: "8080"
	URL:  "http://\(HOST):\(PORT)"

	// Resolved at runtime from 1Password. Never written to disk or your shell.
	DATABASE_PASSWORD: schema.#OnePasswordRef & {
		ref: "op://Engineering/checkout-db/password"
	}
}

tasks: {
	// The three children run in parallel.
	check: schema.#TaskGroup & {
		type: "group"
		lint:  schema.#Task & {command: "npm", args: ["run", "lint"]}
		types: schema.#Task & {command: "npm", args: ["run", "typecheck"]}
		test:  schema.#Task & {command: "npm", args: ["test"]}
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
```

List what the project defines:

```console
$ cuenv task
Tasks from env.cue:
├─ build [1]
└─ check
   ├─ check.lint
   ├─ check.test
   └─ check.types

(5 tasks, 0 groups, 0 cached)
```

Print the resolved environment. Secrets are resolved from your provider at print
time and shown redacted, never in the clear:

```bash
cuenv env print
```

Run a task. `check` runs its three children in parallel; `build` waits for them
to pass and is skipped on a cache hit when nothing changed:

```bash
cuenv task build
```

Run any command inside the same validated environment:

```bash
cuenv exec -- npm start
cuenv exec -e production -- ./deploy.sh
```

## Why CUE?

A `.env` file is just strings, so nothing catches `NODE_ENV=prodction` until
something breaks at runtime. [CUE](https://cuelang.org) is a typed configuration
language: you describe the *shape* of valid config, and evaluation fails loudly
when a value doesn't fit.

```cue
env: {
	NODE_ENV: "development" | "staging" | "production" | *"development" // an enum, default development
	PORT:     >0 & <65536 | *3000                                       // a bounded number, default 3000
	TIMEOUT:  string | *"30s"                                           // any string, default 30s
}
```

Each field declares what counts as valid and what it defaults to. Set
`NODE_ENV: "prod"` and cuenv refuses to run, pointing at `env.NODE_ENV`. CUE also
composes: you can `import` shared definitions and reuse them across services in
a monorepo, instead of copy-pasting `.env` files. Patterns (`=~"^https://"`) and
many other constraints work the same way.

New to CUE? The [CUE site](https://cuelang.org) is the best starting point; you
only need the basics to be productive with cuenv.

## What you can do today

cuenv covers a lot of surface, and not all of it is equally finished. This is an
honest map; the [schema status page](https://cuenv.dev/reference/schema/status/)
is the authoritative source for what's wired up.

**Typed environments.** Constrain values with enums, bounds, regexes, and
defaults. Interpolate between variables. Define per-environment overrides and
select them with `-e production`.

**Runtime secrets.** Secrets are resolved when a command runs, kept out of
`env.cue` and generated files, and redacted from output. Resolvers that work
today: environment variables, any CLI via `#ExecSecret`, 1Password
(`#OnePasswordRef`), and Infisical (`#InfisicalSecret`). `#AwsSecret`,
`#GcpSecret`, and `#VaultSecret` exist in the schema but their runtime resolvers
aren't registered yet — treat them as future work.

**Tasks.** Object keys in a `#TaskGroup` run in parallel; arrays in a
`#TaskSequence` run in order; `dependsOn` uses CUE references so a typo is a
compile error, not a runtime surprise. Tasks support CLI parameters, output
references between tasks, and opt-in content-addressed caching (`cache.mode`).
`timeout`, `retry`, `continueOnError`, and group `maxConcurrency` are in the
schema but not yet fully enforced.

**Shell integration & hooks.** `cuenv shell init <shell>` loads a project's
environment when you `cd` into it, direnv-style. Hooks run in the background
behind an approval gate (`cuenv allow`) so a freshly cloned repo can't run code
without your say-so.

**CI generation.** `cuenv sync ci` turns your pipelines into GitHub Actions
workflows, reusing the same task graph. GitHub is the solid path today;
Buildkite export is partial, and GitLab is schema-only (sync rejects it until an
emitter exists).

**Also in progress.** Multi-source tool management (Nix, GitHub releases, OCI,
URLs), code generation, `.gitignore`/`CODEOWNERS` generation from `.rules.cue`,
release management with changesets, long-running services, and container/Dagger
task runtimes. Check the schema status page before relying on any of these.

## Install

```bash
# Nix
nix profile install github:cuenv/cuenv

# Cargo
cargo install cuenv
```

See [Install cuenv](https://cuenv.dev/how-to/install/) for other platforms,
shell integration, and verification steps.

## CLI at a glance

```bash
cuenv exec -- <command>   # run a command in the validated environment (alias: x)
cuenv task [name]         # list tasks, or run one (alias: t)
cuenv env print           # show the resolved environment (secrets redacted)
cuenv env list            # list available environments
cuenv shell init <shell>  # print shell integration for bash/zsh/fish
cuenv allow / deny        # approve or revoke hook execution for a directory
cuenv sync ci             # generate CI workflows from your pipelines
cuenv fmt                 # format CUE and other configured languages
```

Common flags: `-e/--env <name>` selects an environment, `-p/--path <dir>` points
at the directory holding your CUE files, and `-L/--level <level>` sets log
verbosity. Full details live in the [CLI reference](https://cuenv.dev/reference/cli/).

## Status

| Area                          | Where it stands                                                  |
| ----------------------------- | ---------------------------------------------------------------- |
| CUE evaluation engine         | Solid — fast evaluation through the Go bridge                    |
| Environments & `exec`         | Solid                                                            |
| Tasks (`task`)                | Solid for groups, sequences, deps, params, output refs, caching  |
| Secrets                       | env / exec / 1Password / Infisical / AWS work; GCP and Vault are schema-only |
| Shell integration & hooks     | Solid                                                            |
| CI generation                 | GitHub works; Buildkite partial; GitLab schema-only              |
| Tools, codegen, rules, release| In progress — see the schema status page                         |
| Services, container/Dagger    | Partial — see the schema status page                             |

## How it compares

cuenv overlaps with several tools at once. Roughly:

| Capability             | cuenv               | Make    | Taskfile | direnv  | dotenv |
| ---------------------- | ------------------- | ------- | -------- | ------- | ------ |
| Typed/validated config | yes (CUE)           | no      | no       | no      | no     |
| Environment management | yes, typed          | no      | no       | yes     | yes    |
| Runtime secrets        | yes                 | no      | no       | no      | no     |
| Task dependencies      | yes                 | yes     | yes      | no      | no     |
| Parallel execution     | yes, by default     | `-j`    | limited  | no      | no     |
| Content-addressed cache| yes, opt-in         | no      | no       | no      | no     |
| CI generation          | yes (GitHub today)  | no      | no       | no      | no     |
| Shell integration      | yes                 | no      | no       | yes     | no     |

The point isn't to win every row — it's that cuenv keeps environments, tasks,
and CI in one validated source instead of three that drift.

## Contributing

Contributions are welcome. cuenv is licensed under AGPL-3.0.

```bash
git clone https://github.com/cuenv/cuenv
cd cuenv

# Enter the dev environment (or `direnv allow` if you use direnv)
nix develop

# Project automation lives in cuenv itself
cuenv task fmt.check
cuenv task lint
cuenv task test.unit
cuenv task build
```

See [Develop cuenv](https://cuenv.dev/how-to/develop-cuenv/) and
[Contribute](https://cuenv.dev/how-to/contribute/) for the full workflow.

### Architecture

```
cuenv/
├── crates/
│   ├── cuengine/   # CUE evaluation engine (Go FFI bridge)
│   ├── core/       # Shared types, task execution, caching
│   ├── cuenv/      # CLI and TUI
│   ├── events/     # Event system for UI frontends
│   ├── workspaces/ # Monorepo and package-manager detection
│   ├── ci/         # CI pipeline integration
│   ├── release/    # Version management and publishing
│   ├── codegen/    # CUE-based code generation
│   ├── ignore/     # Ignore-file generation
│   ├── codeowners/ # CODEOWNERS generation
│   ├── github/     # GitHub provider
│   ├── gitlab/     # GitLab provider
│   ├── bitbucket/  # Bitbucket provider
│   └── dagger/     # Dagger task execution backend
├── schema/         # CUE schema definitions
├── examples/       # Runnable CUE configurations
└── docs/           # Documentation site (cuenv.dev)
```

## License

Licensed under the [GNU Affero General Public License v3.0](license.md).

**Why AGPL?** It keeps cuenv open source while leaving room for a sustainable
business: modifications and hosted services built on cuenv stay open source too.

## Links

- **Documentation**: [cuenv.dev](https://cuenv.dev)
- **CUE language**: [cuelang.org](https://cuelang.org)
- **Discussion**: [GitHub Discussions](https://github.com/cuenv/cuenv/discussions)
