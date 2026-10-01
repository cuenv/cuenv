---
title: CLI Reference
description: Complete command-line interface reference for cuenv — every command, subcommand, and flag.
---

The `cuenv` CLI provides tools for managing environments, executing tasks, and integrating with your shell. This page is the authoritative, scannable lookup for every command and flag the binary ships today. Where a feature is partial or schema-only, this page says so explicitly and links to [Schema status](/reference/schema/status/) — never trust a reference that quietly drops behavior on the floor.

## CUE Schema Compatibility

Commands that operate on a CUE module read the module's cuenv schema dependency
from `cue.mod/module.cue` when one is present:

```cue
deps: "github.com/cuenv/cuenv@v0": v: "v0.51.3"
```

If that schema dependency version differs from the running CLI version, cuenv
prints a warning and continues. Missing dependencies are accepted for modules
that vendor schema locally or for the cuenv repository itself. `cuenv sync`
never writes `cue.mod/module.cue`.

## Global Options

These flags are accepted by every subcommand (they are `global = true` in the parser).

| Option        | Description                                            | Default |
| ------------- | ------------------------------------------------------ | ------- |
| `-L, --level` | Set logging level (trace, debug, info, warn, error)    | warn    |
| `--json`      | Emit JSON envelope regardless of the command's format  | false   |
| `-e, --env`   | Apply environment-specific overrides (e.g. production) | none    |
| `--llms`      | Print LLM context information (`llms.txt`) and exit    | false   |

:::caution[Short flag for level is `-L`, not `-l`]
The short flag for `--level` is `-L` (uppercase). The lowercase `-l` short flag is used by `--label` in task, build, and service commands. Use `cuenv -L debug task build`, never `cuenv -l debug ...`.
:::

:::tip
`cuenv --llms` prints a compact `llms.txt`-style context bundle, handy for feeding cuenv's surface to an LLM coding assistant.
:::

## Environment Variables

| Variable             | Description                                                                                                                                                                                       | Default |
| -------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------- |
| `CUENV_EVAL_TIMEOUT` | Seconds to wait for CUE module evaluation before failing with a timeout. Must be a positive whole number; any other non-empty value is a configuration error. An empty value selects the default. | `60`    |

The evaluation timeout only stops a hung evaluation. A cold evaluation that
also fetches registry modules can take several seconds on small CI runners, so
raise `CUENV_EVAL_TIMEOUT` if you see `CUE evaluation timed out after …`.

## Commands

### `cuenv version`

Show version information.

```bash
cuenv version [OPTIONS]
```

**Options:**

- `-o, --output <FORMAT>`: Output format (text, json, env). Default: `text`.

### `cuenv info`

Show module information (bases and projects). With no `PATH`, evaluates the entire module recursively; pass a `PATH` to evaluate a single directory.

```bash
cuenv info [PATH] [OPTIONS]
```

**Arguments:**

- `[PATH]`: Directory to evaluate. If omitted, evaluates the whole module recursively.

**Options:**

- `--package <PACKAGE>`: Name of the CUE package to evaluate. Default: `cuenv`
- `--meta`: Include `_meta` source location for all values (JSON output).

**Examples:**

```bash
# Inspect the whole module: bases and discovered projects
cuenv info

# Evaluate just one directory
cuenv info examples/env-basic

# Emit source locations for every value
cuenv info --meta --json
```

### `cuenv env`

Environment variable operations. Subcommands are listed in source order.

#### `cuenv env print`

Print environment variables from a CUE package.

```bash
cuenv env print [OPTIONS]
```

**Options:**

- `-p, --path <PATH>`: Path to directory containing CUE files. Default: `.`
- `--package <PACKAGE>`: Name of the CUE package to evaluate. Default: `cuenv`
- `-o, --output <FORMAT>`: Output format (env, json, text). Default: `env`

#### `cuenv env load`

Load environment and execute hooks in the background.

```bash
cuenv env load [OPTIONS]
```

**Options:**

- `-p, --path <PATH>`: Path to directory containing CUE files. Default: `.`
- `--package <PACKAGE>`: Name of the CUE package to evaluate. Default: `cuenv`

#### `cuenv env status`

Show hook execution status. With `--wait`, prints the final hook status after hook execution completes or fails.

```bash
cuenv env status [OPTIONS]
```

**Options:**

- `-p, --path <PATH>`: Path to directory containing CUE files. Default: `.`
- `--package <PACKAGE>`: Name of the CUE package to evaluate. Default: `cuenv`
- `--wait`: Wait for hooks to complete before returning.
- `--timeout <SECONDS>`: Timeout in seconds for waiting. Default: `300`
- `-o, --output <FORMAT>`: Output format (text, short, starship). Default: `text`

#### `cuenv env inspect`

Inspect cached hook state for the current config.

```bash
cuenv env inspect [OPTIONS]
```

**Options:**

- `-p, --path <PATH>`: Path to directory containing CUE files. Default: `.`
- `--package <PACKAGE>`: Name of the CUE package to evaluate. Default: `cuenv`

#### `cuenv env check`

Check hook status and output environment for shell.

```bash
cuenv env check [OPTIONS]
```

**Options:**

- `-p, --path <PATH>`: Path to directory containing CUE files. Default: `.`
- `--package <PACKAGE>`: Name of the CUE package to evaluate. Default: `cuenv`
- `--shell <SHELL>`: Shell type for export format (bash, zsh, fish). Default: `bash`

#### `cuenv env list`

List available environments defined in your configuration.

```bash
cuenv env list [OPTIONS]
```

**Options:**

- `-p, --path <PATH>`: Path to directory containing CUE files. Default: `.`
- `--package <PACKAGE>`: Name of the CUE package to evaluate. Default: `cuenv`
- `-o, --output <FORMAT>`: Output format (text, json, env). Default: `text`

### `cuenv task`

Execute a task defined in CUE configuration. Aliased as `cuenv t`.

```bash
cuenv task [NAME] [OPTIONS] [-- TASK_ARGS...]
```

**Arguments:**

- `[NAME]`: Name of the task to execute (lists available tasks if not provided).
- `[TASK_ARGS]`: Arguments to pass to the task (positional and `--named` values).

**Options:**

- `-p, --path <PATH>`: Path to directory containing CUE files. Default: `.`
- `--package <PACKAGE>`: Name of the CUE package to evaluate. Default: `cuenv`
- `-l, --label <LABEL>`: Execute all tasks matching the given label (repeatable, AND semantics).
- `-o, --output <FORMAT>`: Output format for task listing (defaults to config or TTY auto-detect).
- `--tui`: Use the rich TUI for task execution.
- `-i, --interactive`: Interactive task picker — select a task to run.
- `-S, --skip-dependencies`: Skip executing task dependencies (for CI orchestrators that handle deps externally).
- `--continue-on-error`: Don't abort on first failure; dependents of the failing task are emitted as `task.skipped` and unrelated siblings keep running.
- `-n, --dry-run`: Export the task dependency graph as JSON without executing tasks.
- `--materialize-outputs <DIR>`: Materialize cached outputs to this directory on cache hit (off by default).
- `--show-cache-path`: Print the cache path for this task key.
- `--backend <BACKEND>`: Force a specific execution backend (`host` or `dagger`).
- `--help`: Print task-specific help (when a task name is provided).

:::note[Backend status]
The `dagger` backend is optional and gated behind the `dagger-backend` build feature. Host execution is the default and fully supported. See [Schema status](/reference/schema/status/) for current backend coverage.
:::

**Label-based execution:**

You can execute multiple tasks at once using labels:

```bash
# Execute all tasks with the 'test' label
cuenv task -l test

# Execute tasks matching both 'test' AND 'unit' labels
cuenv task -l test -l unit
```

Labels are defined in your CUE task configuration and allow grouping related tasks across projects.

**Inspecting the plan without running it:**

```bash
# Print the resolved task DAG as JSON and exit (no execution)
cuenv task build --dry-run

# Pick a task interactively
cuenv task --interactive
```

:::tip
Use the global `-e` flag to apply environment-specific overrides: `cuenv -e production task build`.
:::

### `cuenv exec`

Execute a command with CUE environment variables. Aliased as `cuenv x`.

```bash
cuenv exec [OPTIONS] -- <COMMAND> [ARGS]...
```

**Options:**

- `-p, --path <PATH>`: Path to directory containing CUE files. Default: `.`
- `--package <PACKAGE>`: Name of the CUE package to evaluate. Default: `cuenv`

:::tip
Use the global `-e` flag to apply environment-specific overrides: `cuenv -e production exec -- npm start`.
:::

`cuenv exec` does not inherit the caller's `HOME`. It uses a private, writable
home under cuenv's cache directory when that location is writable, and falls
back to a per-user directory under the system temporary directory in sandboxed
builds. Its default XDG config, cache, data, and state directories are placed
beneath that home. Values declared in the project environment take precedence.

### `cuenv fmt`

Format code based on your project's formatters configuration.

```bash
cuenv fmt [OPTIONS]
```

**Options:**

- `--fix`: Apply formatting changes. Without this flag, runs in check mode (validates without modifying).
- `--only <FORMATTERS>`: Run only specific formatters (comma-separated). Valid values: `rust`, `nix`, `go`, `cue`.
- `-p, --path <PATH>`: Path to directory containing CUE files. Default: `.`
- `--package <PACKAGE>`: Name of the CUE package to evaluate. Default: `cuenv`

**Examples:**

```bash
# Check formatting (default - exits non-zero if issues found)
cuenv fmt

# Apply formatting fixes
cuenv fmt --fix

# Check only Rust and Go files
cuenv fmt --only rust,go

# Fix only Nix files
cuenv fmt --fix --only nix

# Format files in a specific project
cuenv fmt --fix -p ./packages/my-app
```

**Exit Codes:**

- `0`: All files are properly formatted (check mode) or formatting succeeded (fix mode)
- `3`: Files need formatting (check mode) or formatter error

:::note
The `cuenv fmt` command requires a `formatters` block in your `env.cue`. See the [Formatters Guide](/how-to/formatters/) for configuration details.
It discovers files once with the repository ignore rules applied, then dispatches
the matched Rust, Nix, Go, and CUE file groups through the shared formatter
runners used by sync checks.
:::

### `cuenv build`

List or build container images defined in CUE configuration.

```bash
cuenv build [NAMES...] [OPTIONS]
```

**Arguments:**

- `[NAMES]`: Image names to build. Omit names to list available images.

**Options:**

- `-p, --path <PATH>`: Path to directory containing CUE files. Default: `.`
- `--package <PACKAGE>`: Name of the CUE package to evaluate. Default: `cuenv`
- `-l, --label <LABEL>`: Filter images by label (repeatable).

Selected images are built with the local Docker CLI. Dockerfile images (with
`context`) use `docker build`, or `docker buildx build --push` when `registry`
is configured; local multi-platform builds require a registry. Nix images (with
`installable`) are built with `nix build`, loaded via `docker load`, then tagged
and pushed with `docker`.

:::note[Image backends are partial]
`#ContainerImage` is partially implemented: `cuenv build` lists and builds images with the local Docker CLI, and registry builds are pushed with Docker buildx. Dagger execution and downstream image output-reference resolution remain incomplete. See [Schema status](/reference/schema/status/).
:::

**Examples:**

```bash
# List all images
cuenv build

# Build the api image with the local Docker CLI
cuenv build api

# Build all images with the ci label
cuenv build --label ci
```

### `cuenv up`

Bring up long-running services defined in CUE configuration.

```bash
cuenv up [SERVICES...] [OPTIONS]
```

**Arguments:**

- `[SERVICES]`: Service names to bring up. If not provided, starts all services.

**Options:**

- `-p, --path <PATH>`: Path to directory containing CUE files. Default: `.`
- `--package <PACKAGE>`: Name of the CUE package to evaluate. Default: `cuenv`
- `-l, --label <LABEL>`: Filter services by label (repeatable).
- `-e, --env <NAME>` _(global)_: Select an environment (e.g., `test`, `production`). Project-level env values for that environment are merged into each service's env (service-level entries win).

Services are supervised processes with readiness probes, restart policies, and file watchers. Use `Ctrl+C` to shut down all services.

Service-to-service dependencies wait for upstream services to become ready;
starting a selected service also starts its service dependencies. Task
dependencies in `services.*.dependsOn` run to completion before service
startup. Image dependencies are recognized in the dependency plan, but selected
image builds still fail fast until image execution backends exist.

On Linux, `cuenv up` promotes itself to a subreaper (`PR_SET_CHILD_SUBREAPER`) and installs `PR_SET_PDEATHSIG=SIGKILL` on each service so orphaned descendants are either reaped by cuenv or killed when cuenv exits. On macOS, services are spawned through a hidden `cuenv __supervise` wrapper that watches the parent via `kqueue`/`NOTE_EXIT` and forwards signals to the service's process group.

Secrets referenced by a service's `env` (including values inherited from the project-level environment) are resolved in parallel before the service starts and registered with the event system for redaction.

**Examples:**

```bash
# Start all services
cuenv up

# Start specific services
cuenv up api db

# Start services matching a label
cuenv up --label backend

# Start services with the "test" environment applied
cuenv up -e test
```

### `cuenv down`

Request graceful shutdown of the active service session.

```bash
cuenv down [SERVICES...] [OPTIONS]
```

`cuenv down` signals the persisted `cuenv up` controller for the selected
project path. Omit service names to stop the whole active session. Provide
service names to queue persisted stop requests for those running supervisors.

**Arguments:**

- `[SERVICES]`: Service names to stop. Omit to stop the whole active session.

**Options:**

- `-p, --path <PATH>`: Path to directory containing CUE files. Default: `.`
- `--package <PACKAGE>`: Name of the CUE package to evaluate. Default: `cuenv`

### `cuenv logs`

View service logs.

```bash
cuenv logs [SERVICES...] [OPTIONS]
```

**Arguments:**

- `[SERVICES]`: Service names to view logs for. If not provided, shows all.

**Options:**

- `-p, --path <PATH>`: Path to directory containing CUE files. Default: `.`
- `--package <PACKAGE>`: Name of the CUE package to evaluate. Default: `cuenv`
- `-f, --follow`: Stream appended persisted log lines until the active service session exits.
- `-n, --lines <N>`: Number of lines to show before following. Default: `100`

### `cuenv ps`

List running services and their status.

```bash
cuenv ps [OPTIONS]
```

**Options:**

- `-p, --path <PATH>`: Path to directory containing CUE files. Default: `.`
- `--package <PACKAGE>`: Name of the CUE package to evaluate. Default: `cuenv`
- `--output <FORMAT>`: Output format (table, json). Default: `table`

### `cuenv restart`

Restart one or more services.

```bash
cuenv restart <SERVICES...> [OPTIONS]
```

`cuenv restart` requires an active `cuenv up` session for the selected project.
It queues a persisted restart request for each named service, and the running
supervisor consumes that request to stop and re-spawn the service.

**Arguments:**

- `<SERVICES>`: Service names to restart (required).

**Options:**

- `-p, --path <PATH>`: Path to directory containing CUE files. Default: `.`
- `--package <PACKAGE>`: Name of the CUE package to evaluate. Default: `cuenv`

### `cuenv infrastructure`

Plan and apply infrastructure declared in the project's `infrastructure` block by
driving Terraform provider plugins over gRPC. State is stored in the
configured Turso database, keyed by the CUE module path, the project name and
the selected environment, and owned by one CUE instance (the project's
directory and package).
See [Manage infrastructure](/how-to/infrastructure/).

:::caution[Experimental]
This command is experimental, and `cuenv infrastructure --help` says so. Its
behavior, flags and state layout can change between releases, and it is not
covered by the stability promises of the other commands.
:::

`cuenv i` is the short form of `cuenv infrastructure` (for example
`cuenv i apply`).

```bash
cuenv infrastructure plan    [OPTIONS]
cuenv infrastructure apply   [OPTIONS] [--yes] [--allow-separate-state]
cuenv infrastructure destroy [OPTIONS] [--yes]
cuenv infrastructure state   [list | locks | remove <ADDRESS> | recover [--force] [--accept-backend] | adopt] [OPTIONS]
cuenv infrastructure unlock  [OPTIONS] [--module <MODULE>] [--project <PROJECT>] [LOCK_IDENTIFIER]
```

- `plan`: refresh recorded resources and show the changes `apply` would make,
  including records it would refresh ("N to refresh"), in the order `apply`
  would run them. Dependency cycles and changes that no order can apply are
  refused here, before anything changes. It takes no lock.
- `apply`: converge on the configuration. Resources removed from
  `infrastructure.resources` are deleted. As in Terraform, `apply` takes the
  project's state lock, plans, shows the plan and asks for confirmation
  **while holding the lock**, then applies exactly the plan it showed; there
  is no second plan. Answering anything but `yes`, or closing standard input,
  cancels (exit code `1`, nothing applied) and releases the lock; so does an
  interrupt at the prompt (exit code `130`). A signal that arrives while the
  last operation is in flight lets it finish and be recorded, and the run still
  exits `130` (`infrastructure_interrupted`) saying what was applied. With `--yes` there is no prompt:
  lock, plan, show the plan, apply. A plan with nothing to do asks nothing.
  The lock is taken whenever the plan has work, including records that only
  need refreshing. Operations run as one dependency graph (a replacement is a
  delete followed by a create): when a provider fails one, everything that
  depends on it is skipped and the rest still run, and the command exits `5`
  listing the failures, the skipped changes and every replacement that was
  deleted but not recreated (also in the JSON error envelope as
  `deletedNotRecreated`); the next `apply` creates those.
- `destroy`: the same flow, deleting every managed resource recorded for the
  project.
- `state` or `state list`: list managed resources recorded for the project,
  marking tainted ones.
- `state locks`: list every lock held in the state database, for every
  project, module and environment (not only the evaluated project's), oldest
  first: module, project, environment, lock identifier, holder, when it was
  acquired and how long ago, and the `unlock` command that releases it. It
  reads only, evaluates the current project just to find the database, and
  never creates tables.
- `state remove <ADDRESS>`: forget one managed resource (such as
  `random_pet.pet`) under the lock, without touching the real object — the
  escape hatch when its provider is gone, or when its record is damaged: it
  reads only the address columns, so a record whose content cannot be decoded
  can still be removed. An address that is not recorded is refused (exit code
  `2`).
- `state recover`: record every change an earlier run could not record and
  saved in the user state directory
  (`$XDG_STATE_HOME/cuenv/infrastructure/unrecorded/`, by default
  `~/.local/state/…` on Linux), under the lock, deleting each file once it is
  recorded. With nothing to recover it takes no lock. A saved change is
  written only while the stored generation and serial still match the record
  it replaced; deleting and recreating an address changes its generation. If
  another run changed the stored record since, recovery stops (exit code `5`)
  and names the file, the address and the reason. Then either run
  `state recover --force`, which writes the saved record over the newer one,
  or move the saved file out of that directory to keep the stored record.
  Each file also binds to the state backend that could not record it
  (`localhost`, `127.0.0.1` and `[::1]` count as one backend): a file saved for
  a different backend, or without a binding, is refused until you inspect it and
  run `state recover --accept-backend`. The two overrides are independent and
  evaluated per file; neither implies the other. `plan`, `apply` and `destroy`
  refuse to run (exit code `5`) while such files exist for the project and
  environment. A file that cannot be used (unreadable, malformed, another
  tenant's, not private to you) is named in the error, which says to move it
  aside. There is one file format (version 1). Without `--env`, a note lists the
  declared environments that have unrecorded changes.
- `state adopt`: make this project's CUE instance (`<directory>:<package>`)
  the owner of its state, under the lock, and print the previous and the new
  owner. Use it after moving a project to another directory or package.
- `unlock`: without an identifier, show who holds the lock and since when
  (exit `0`, also when no lock is held; without `--env` it also lists locks held
  in the project's declared environments); with one, release exactly that lock.
  Locks are per identity, so pass the `--env` of the run that holds it. An
  identifier that matches no lock (already released, or held in another
  environment, which the error says) exits with code `2` and releases nothing;
  naming a lock that is not the current one releases nothing and exits with
  code `4`. With `--module` and/or `--project` it acts on the lock of that
  project (the evaluated project's module path or name for whichever is
  omitted) instead of the evaluated project's: the locked project need not
  exist or evaluate, which is how a lock left by a dead run of another project
  sharing the database, or by a deleted one, is released (`state locks` lists
  them and prints the exact command). `--env` still selects the environment.

**Options:**

- `-p, --path <PATH>`: Path to directory containing CUE files. Default: `.`
  (for `state`, accepted before or after its subcommand).
- `-e, --env <NAME>` _(global)_: Select the complete configuration at
  `infrastructure.environments.NAME` and the project values at
  `env.environment.NAME`. It replaces the top-level configuration entirely;
  nothing is merged or inherited. Without it the run uses the top-level
  `providers`, `resources` and `providerEnvironment`. Each name, and the run
  without `--env`, is a separate state identity (resources, lock, owner and
  recovery files) in the one state database. An unknown name fails before the
  state backend is contacted and lists the declared environments (exit code
  `2`). See "Environment selection" below.
- `--package <PACKAGE>`: Name of the CUE package to evaluate. Default: `cuenv`
- `-y, --yes` (`apply`, `destroy`): Skip the confirmation prompt. Required
  when standard input is not a terminal, and with `--json`. `--auto-approve`
  is accepted as an alias.
- `--allow-separate-state` (`apply`): with `--env NAME`, create resources
  whose addresses the project also has recorded without `--env`, as separate
  objects, instead of refusing (see "Environment selection").
- `--module <MODULE>`, `--project <PROJECT>` (`unlock`): the CUE module path
  and project name of the lock to show or release, when it is not the
  evaluated project's.
- `--force` (`state recover`): write a saved record even over a stored record
  that changed since it was saved.
- `--accept-backend` (`state recover`): accept a saved record that was saved for
  a different state backend than the one in use, or without a backend binding.

**Environment selection:** with `--env NAME` the selected configuration must
be complete and concrete. State is recorded separately for each name and for a
run without `--env`, so top-level `resources` and `environments` with
non-overlapping resource addresses coexist, and the command refuses (exit code
`2`, before any provider starts, after the state store is read) the runs that
would act on the wrong identity. The help of each refusal names only commands
that work from the state recorded at that moment:

- `apply --env NAME` when it would create a resource whose address
  (`type.name`) is also recorded without `--env` and not yet recorded for
  `NAME`: both identities would claim the same real objects. `--allow-separate-state`
  creates them as separate objects anyway. `plan --env NAME` only warns, and
  `destroy --env NAME` never refuses. Moving records (`state move`) is not
  available yet; the error lists the alternatives.
- `apply` without `--env` when `infrastructure.environments` is declared and
  there are no top-level `resources`; `plan` warns. `destroy` without `--env`
  in that layout runs when resources are still recorded without `--env` and
  the top-level `providers` that delete them are declared, which is the way out
  of an old layout, and refuses otherwise.

The state commands need only `infrastructure.state`, so `state list`,
`state locks`, `state remove`, `state recover`, `state adopt` and `unlock` work
for an environment that is no longer declared (with a warning) and while the
providers and resources are incomplete. When the state database holds no
records, lock, owner or saved unrecorded change for the `--env` name, and the
project does not declare it, they refuse instead (exit code `2`, listing the
declared environments): a mistyped name is not a removed environment. They
still fail when the project does not evaluate at all (see below). Every
command named in an error or hint carries the run's `--env`, `-p` and
`--package`.

**Provider environment and policy:** provider processes inherit the cuenv
process environment minus the credentials of cuenv's secret resolvers
(`OP_SERVICE_ACCOUNT_TOKEN`, `INFISICAL_TOKEN`, `INFISICAL_CLIENT_ID`,
`INFISICAL_CLIENT_SECRET`, `VAULT_TOKEN`) and the state token, unless the
project passes a variable of that name. `providerEnvironment: "isolated"`
(top level or per environment) starts from an empty environment instead. The
project's variables that the action's `allowInfrastructure` policy allows are
added in both modes. See
[Manage infrastructure](/how-to/infrastructure/#provider-environment).

**Project name uniqueness:** state is keyed by the CUE module path and the
project name, so `plan`, `apply`, `destroy` and `state adopt` evaluate every
instance of every CUE package in the module first (exporting only each
instance's `name` and whether it has an `infrastructure` block). They refuse
to run when another instance declares the same project name together with an
`infrastructure` block (exit code `2`), when any instance fails to evaluate
(exit code `3`; the error names each failing instance as
`<directory>:<package>`), because a broken instance could hide a duplicate,
and when the module-wide evaluation does not include the project itself
(exit code `2`). CUE's `./...` pattern leaves out directories whose name
starts with `.` or `_`, directories named `testdata` and nested modules, so a
project inside one of them could not be checked; the error names the
directory and why it is left out. A directory below a project inherits its
`name` and `infrastructure` in CUE and cannot set another `name` in the same
package, so it counts as a duplicate: put its files in a different CUE
package or move them out from under the project. `state list`,
`state remove`, `state recover` and `unlock` evaluate only the project, so
they keep working while a sibling is broken. Only `plan`, `apply` and
`destroy` require the selected `infrastructure` configuration to be concrete;
the state commands require only `infrastructure.state`. Other commands keep
infrastructure as raw configuration, so unused incomplete named environments
do not prevent ordinary task discovery, sync or CI decoding. The schema's own
checks are different: a semantic error (a provider without `version` or `path`,
an undeclared `dependsOn` or provider, an invalid state URL) in any
environment, selected or not, is a CUE evaluation error and fails every
command that evaluates the project, deliberately fail-closed (exit code `3`).

**Ownership:** the first `apply` or `destroy` records the project's CUE
instance as the owner of its state, under the lock. `plan`, `apply`,
`destroy`, `state remove` and `state recover` with pending changes from any
other instance with the same module path, project name and selected environment
(for example a nested module declaring the same module path) are refused
(exit code `5`); the error points at `state adopt`. `state remove` and
`state recover` check the recorded owner after taking the lock, before changing
state or deleting saved recovery files; `--force` does not bypass ownership.
These commands keep unowned state unowned. `state list`, `unlock` and
`state recover` with nothing to recover do not check the owner.

**Tables and read-only access:** every write happens under the lock, and the
state tables are created or upgraded right before the lock is taken. A schema
migration after the first refuses while any lock is held, waits up to 30
seconds for the locks to be released (holding new locks off meanwhile), and
then fails with exit code `4` listing every blocking lock and the command that
releases it. A database holding tables of an unreleased development build, or
tables with cuenv's names that it did not create, is refused by name. `plan`,
`state list`, `state locks`, `unlock` (with or without an identifier) and a
`state recover` with nothing to recover never create or upgrade the tables, so they work with a
read-only token (except for releasing a lock) and against a database cuenv
has never written.

**JSON output:** with the global `--json`, every subcommand prints exactly one
JSON document on standard output and nothing else, once the command is over
(after the lock is released); progress, warnings and other events, including
the `Acquired lock …` and `Released lock …` lines, go to standard error as
JSON lines. On failure the document is the error envelope
`{"status":"error","error":{"code","message","help","lockIdentifier","lockReleased"}}`:
`help` is present when there is help text; `lockIdentifier` and
`lockReleased` are present when the failure concerns a state lock (this run's
lock, or the one that blocked it), and `lockReleased` is `false` whenever the
release failed or is not known; `deletedNotRecreated` (an array of addresses)
is present when a failed or interrupted apply left replacements deleted and
not recreated. A run that succeeded but could not release its lock reports
that as an error, not as success. Every string in the envelope is redacted
for registered secrets as a string. Payloads never contain attribute values.
Every payload below carries `tenant` (module path and project,
`<module>#<project>`) and `environment` (the `--env` name, or `null` without
`--env`) as separate fields, plus:

- `plan`: `changes` (`address`, `action`, `requiresReplace`, in apply order)
  and `summary` (`create`, `update`, `replace`, `delete`, `refresh`,
  `unchanged`). `action` is one of `no-op`, `refresh`, `create`, `update`, `replace`,
  `delete`.
- `apply`, `destroy`: the applied plan in the same shape, plus `operation`
  (`apply` or `destroy`) and `applied` (the counts applied, or `null` when there
  was nothing to do). `--json` requires `--yes` because the confirmation
  prompt cannot share standard output with the result.
- `state`, `state list`: `resources` (`address`, `provider`,
  `providerSource`, `schemaVersion`, `tainted`).
- `state locks`: `locks` (`module`, `project`, `environment` or `null`,
  `lockIdentifier`, `holder`, `acquiredAt`, `unlockCommand`).
- `state remove`: `removed` (the address).
- `state recover`: `recovered` (the addresses recorded).
- `state adopt`: `previousOwner` (`instance`, `claimedAt`, or `null`)
  and `owner` (`instance`, `claimedAt`).
- `unlock`: `lock` (`lockIdentifier`, `holder`, `acquiredAt`, or
  `null` when unlocked) and `released`.

`state list`, `state recover` and `unlock` without `--env` also carry
`otherEnvironments`, an array (empty when there is nothing to say) with one
entry per declared environment worth mentioning: `environment`, `command` (the
command that deals with it) and `resources` (a count, `state list`),
`unrecordedChanges` (`true`, `state recover`) or `lockIdentifier` and `holder`
(`unlock`). The same findings are printed as notes on standard error.

The error envelope can also carry `result` (when an operation finished but its
lock could not be released: the document the operation would have printed, such
as the applied counts, or the counts of an apply interrupted after its last
operation) and `blockingLocks` (when a schema migration is blocked by held
locks: `module`, `project`, `environment`, `lockIdentifier`, `holder`,
`acquiredAt`, `unlockCommand`).

**Interrupts:** the command handles Ctrl-C (SIGINT), SIGTERM, SIGHUP (the
terminal closing) and SIGQUIT itself for its whole run, as Terraform does.
The first signal starts no new resource and asks every running provider to
stop, so the operation in flight returns early; whatever it returns is
recorded (an interrupted create is recorded as tainted), the lock is released
and the exit code is `130`. CUE evaluation, reading state and the prompt are
abandoned at once; planning stops at the next resource. If the lock is being
acquired, acquisition finishes and the lock is released again. A second
signal kills every provider process, gives a record being written up to about
two seconds, releases the lock if that takes at most about two seconds, and
exits with code `130` after writing the outcome and the lock identifier
directly to standard error (in JSON mode as a JSON event line, plus the one
error envelope on standard output, with `lockIdentifier` and
`lockReleased`). If the second signal arrives while the lock is being
acquired, the message says the lock may have been acquired and how to release
it. The lock identifier is chosen before it is requested, and each run prints
`Acquired lock <identifier>` on standard error when it gets the lock.
Provider processes run in their own process group, so a terminal Ctrl-C
reaches only cuenv.

**Lock holder:** a lock records the subcommand, the user (the account of the
real user identifier), the host name, the process identifier and, in GitHub
Actions, the workflow run URL. `unlock` without an identifier shows it, with
control characters removed.

**Exit codes:**

| Code  | Meaning                                                                                                                                                                                                                                                                                                                                                                              | JSON `code`                  |
| ----- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ---------------------------- |
| `0`   | Success, including `unlock` when no lock is held                                                                                                                                                                                                                                                                                                                                     | (success envelope)           |
| `1`   | The plan was not confirmed at the prompt (declined, or standard input ended or could not be read); nothing was applied and the lock was released                                                                                                                                                                                                                                     | `infrastructure_cancelled`   |
| `2`   | Configuration error: a duplicate project name, a project the module-wide evaluation leaves out, an unknown `--env` name, a run refused for acting on the wrong state identity (see Environment selection), an unknown `state remove` address, `unlock <identifier>` that matches no lock, a state URL the Rust parser rejects, `--json` without `--yes`, a prompt without a terminal | `config`                     |
| `3`   | CUE evaluation error, including an instance in the module that fails to evaluate, and a secret that cannot be resolved (the error names the variable)                                                                                                                                                                                                                                | `eval`                       |
| `4`   | Concurrent activity; retrying later can succeed: the state is locked by another run, a schema migration is blocked by held locks or waiting for them, or `unlock` named a lock that is not the current one                                                                                                                                                                                                                                            | `infrastructure_locked`      |
| `5`   | Any other infrastructure failure: provider, state store, apply, another instance owning the state, unrecorded changes pending or unusable, a recovery that would overwrite a newer record, a change that was not recorded, a lock that could not be released                                                                                                                         | `infrastructure`             |
| `130` | Interrupted by a signal; everything applied is recorded. The lock is released, or the error says it was not and how to release it                                                                                                                                                                                                                                                    | `infrastructure_interrupted` |

The Turso authentication token is read from the environment variable named by
`infrastructure.state.turso.authenticationTokenEnvironmentVariable` (default
`TURSO_AUTH_TOKEN`) and withheld from provider processes. Provider downloads
are cached in `TF_PLUGIN_CACHE_DIR` when set. Logs of the infrastructure
engine follow `--level` like the rest of cuenv.

### `cuenv shell`

Shell integration commands.

#### `cuenv shell init`

Generate shell integration script.

```bash
cuenv shell init <SHELL>
```

**Arguments:**

- `<SHELL>`: Shell type (fish, bash, zsh).

### `cuenv allow`

Approve configuration for hook execution.

```bash
cuenv allow [OPTIONS]
```

**Options:**

- `-p, --path <PATH>`: Path to directory containing CUE files. Default: `.`
- `--package <PACKAGE>`: Name of the CUE package to evaluate. Default: `cuenv`
- `--note <NOTE>`: Optional note about this approval.
- `-y, --yes`: Approve without prompting.

### `cuenv deny`

Revoke approval for hook execution.

```bash
cuenv deny [OPTIONS]
```

**Options:**

- `-p, --path <PATH>`: Path to directory containing CUE files. Default: `.`
- `--package <PACKAGE>`: Name of the CUE package to evaluate. Default: `cuenv`
- `--all`: Revoke all approvals for this directory.

### `cuenv ci`

Run CI pipelines defined in your CUE configuration.

```bash
cuenv ci [OPTIONS]
```

**Options:**

- `-p, --pipeline <NAME>`: Force a specific pipeline to run (defaults to `default`).
- `--export <FORMAT>`: Export pipeline YAML instead of running (`buildkite`, `gitlab`, `github-actions`, `circleci`).
- `-o, --output <PATH>`: Write exported YAML to a file instead of stdout.
- `-j, --jobs <N>`: Maximum parallel task DAG jobs. `0` uses host parallelism.
- `--from <REF>`: Base ref to compare against (branch name or commit SHA) for affected detection.
- `--dry-run`: Show what would be executed without running it.
- `-e, --environment <NAME>`: Environment for secrets resolution.
- `--filter-matrix <KEY=VALUE>`: Reserved for local runner matrix filtering. Currently rejected; use `cuenv sync ci` for provider-native matrix workflows.

:::caution[GitLab export is schema-only]
`--export gitlab` (and `cuenv sync ci --provider gitlab`) is schema-recognized only. There is no GitLab emitter yet, so sync rejects it with a configuration error. GitHub Actions is the strongest path; Buildkite export/sync is partial. See [Schema status](/reference/schema/status/).
:::

**Example:**

```bash
# Run CI pipeline
cuenv ci

# See what would run without executing
cuenv ci --dry-run

# Output dynamic Buildkite pipeline (pipe to buildkite-agent)
cuenv ci --export buildkite | buildkite-agent pipeline upload

# Compare against a specific base ref
cuenv ci --from main
```

For generating committed workflow files (rather than running or exporting on the fly), use [`cuenv sync ci`](#cuenv-sync) below.

### `cuenv sync`

Synchronize generated files from CUE configuration. When run without a
subcommand, executes all sync operations.

```bash
cuenv sync [OPTIONS] [SUBCOMMAND]
```

`cuenv sync` does not update CUE module dependencies. Use `cue mod get
github.com/cuenv/cuenv@<version>` when you want to change the schema dependency
recorded in `cue.mod/module.cue`.

**Options:**

- `-p, --path <PATH>`: Path to directory containing CUE files. Default: `.`
- `--package <PACKAGE>`: Name of the CUE package to evaluate. Default: `cuenv`
- `--dry-run`: Show what would be generated without creating files.
- `--check`: Check if files are in sync without making changes (exits with error if out of sync).
- `-A, --all`: Sync all projects in the workspace.

**Subcommands:**

- `lock`: Resolve tools, artifacts, and runtimes (including OCI image digests) into `cuenv.lock`. Supports `--dry-run`, `--check`, `-A/--all`, and `-u/--update [TOOLS...]` (omit names to update all).
- `codegen`: Sync files from CUE codegen configurations. Adds `--diff` to show changed files.
- `ci`: Sync CI workflow files from CUE configuration. Adds `--provider <github|buildkite>` to filter.
- `vcs`: Sync cuenv-managed Git dependencies. Supports `-u/--update [NAMES...]` to refresh locked refs.

**Example:**

```bash
# Run all sync operations
cuenv sync

# Resolve lockfile state only
cuenv sync lock

# Update specific locked tools
cuenv sync lock -u bun jq

# Preview what would be generated
cuenv sync --dry-run

# Check if all files are in sync (for CI)
cuenv sync --check

# Sync codegen with diff output
cuenv sync codegen --diff

# Generate configured CI workflow files
cuenv sync ci

# Check workflows are in sync (CI validation)
cuenv sync ci --check

# Sync VCS dependencies
cuenv sync vcs

# Update locked VCS refs
cuenv sync vcs --update

# Sync all projects in the workspace
cuenv sync --all
```

**Output Status:**

- `Created` - New file was created
- `Updated` - Existing file was updated
- `Unchanged` - File content unchanged, no write needed

**Workflow generation:**

cuenv can generate CI workflow files for different providers via `cuenv sync ci`:

- **GitHub Actions**: Creates `.github/workflows/*.yml` files with monorepo-aware naming.
- **Buildkite**: Creates `.buildkite/pipeline.yml` bootstrap or outputs dynamic YAML.
- **GitLab**: Schema-recognized only; `cuenv sync ci --provider gitlab` exits with a configuration error until a GitLab emitter exists.

The `--check` flag validates that generated workflows match existing files, exiting with an error if they differ — useful for enforcing workflow consistency in CI.

**Security:**

- Must be run within a Git repository.
- Tool names cannot contain path separators or `..`.
- Files are only written within the Git repository.

**Configuration:**

Add an `ignore` field to your `env.cue`:

```cue
ignore: {
    // Simple format: list of patterns
    git: ["node_modules/", ".env", "*.log"]
    docker: ["node_modules/", ".git/", "target/"]

    // Extended format: custom filename
    custom: {
        patterns: ["*.tmp", "cache/"]
        filename: ".myignore"
    }
}
```

Tool names map to ignore files as `.{tool}ignore` (e.g., `git` creates `.gitignore`, `docker` creates `.dockerignore`). Use the extended format with `filename` to override this default.

See the [Ignore Files guide](/how-to/ignore-files/) for more details.

### `cuenv runtime`

Runtime management commands.

#### `cuenv runtime oci activate`

Activate OCI binaries for the current environment. This runs the `#OCIActivate` hook: it extracts the binaries that `cuenv sync lock` resolved into `cuenv.lock` and prepends them to `PATH`.

```bash
cuenv runtime oci activate
```

:::note[OCI runtime is partial]
`#OCIRuntime` is partial. `cuenv sync lock` resolves digests and per-image `extract` paths into `cuenv.lock`; `cuenv runtime oci activate` extracts those binaries and prepends them to `PATH`. See [Schema status](/reference/schema/status/) for the current limitations.
:::

### `cuenv tools`

Manage project tools defined in CUE configuration.

:::note
When using `cuenv exec` or `cuenv task`, tools are activated automatically from the lockfile. The `cuenv tools` commands are for manual management and scripting.
:::

#### `cuenv tools download`

Download tools for the current platform from the lockfile.

```bash
cuenv tools download
```

Downloads all tools specified in `cuenv.lock` for the current platform. Tools are cached in `~/.cache/cuenv/tools/` and reused across projects.

#### `cuenv tools activate`

Output shell export statements to add tool binaries to PATH.

```bash
cuenv tools activate
```

Outputs `export PATH=...` and library path statements (`DYLD_LIBRARY_PATH` on macOS, `LD_LIBRARY_PATH` on Linux).

**Example:**

```bash
# Activate tools in a script
eval "$(cuenv tools activate)"

# Verify tools are available
jq --version
```

:::tip
For `cuenv exec` and `cuenv task`, tools are activated automatically. Use `cuenv tools activate` for manual activation in scripts or when configuring shell hooks.
:::

#### `cuenv tools list`

List configured tools and their platforms.

```bash
cuenv tools list
```

Shows all tools from `cuenv.lock` with their versions, providers, and digests per
platform. The current platform is marked with `(current)`. The command also prints
the current-platform activation preview, including inferred or explicit env
mutations and inline activation errors when the lockfile metadata is invalid.

**Example output:**

```
Tools (3 tools, 3 platforms):

jq (1.7.1):
  darwin-arm64 (current): homebrew sha256:abc123...
  darwin-x86_64: homebrew sha256:def456...
  linux-x86_64: homebrew sha256:789xyz...

yq (4.44.6):
  darwin-arm64 (current): homebrew sha256:...
  ...
```

### `cuenv secrets`

Manage secret provider integrations.

#### `cuenv secrets setup`

Set up a secret provider by downloading required components.

```bash
cuenv secrets setup <PROVIDER> [OPTIONS]
```

**Arguments:**

- `<PROVIDER>`: Provider to set up. Currently supported: `onepassword`, `infisical`

**Options:**

- `--wasm-url <URL>`: Override the default WASM URL (for providers that use a WASM setup step, such as 1Password).

**Example:**

```bash
# Set up 1Password WASM SDK for HTTP mode
cuenv secrets setup onepassword
```

This downloads the 1Password WASM SDK to enable HTTP-based secret resolution. When `OP_SERVICE_ACCOUNT_TOKEN` is set, cuenv uses this for faster, batched secret resolution instead of the `op` CLI. For Infisical, setup validates that `INFISICAL_CLIENT_ID` and `INFISICAL_CLIENT_SECRET`, or `INFISICAL_TOKEN`, are present.

See the [Secrets Guide](/how-to/secrets/) for more details on secret management.

### `cuenv changeset`

Manage changesets for release management.

#### `cuenv changeset add`

Add a new changeset. On a TTY, omitting arguments launches an interactive picker for summary, description, and package bumps.

```bash
cuenv changeset add [OPTIONS]
```

**Options:**

- `-p, --path <PATH>`: Path to project root. Default: `.`
- `-s, --summary <SUMMARY>`: Summary of the change (interactive if omitted).
- `-d, --description <DESC>`: Detailed description of the change.
- `-P, --packages <PACKAGE:BUMP>`: Package and bump type (format: `package:bump`, e.g., `my-pkg:minor`). Repeatable. Interactive if omitted.

**Example:**

```bash
# Non-interactive
cuenv changeset add -s "Add new feature" -P my-pkg:minor -P other-pkg:patch

# Interactive picker (on a TTY)
cuenv changeset add
```

#### `cuenv changeset status`

Show pending changesets.

```bash
cuenv changeset status [OPTIONS]
```

**Options:**

- `-p, --path <PATH>`: Path to project root. Default: `.`
- `--json`: Output in JSON format for CI consumption.

#### `cuenv changeset from-commits`

Generate a changeset from conventional commits.

```bash
cuenv changeset from-commits [OPTIONS]
```

**Options:**

- `-p, --path <PATH>`: Path to project root. Default: `.`
- `-s, --since <TAG>`: Tag to start from. Default: latest tag.

**Example:**

```bash
# Derive a changeset from commits since the latest tag
cuenv changeset from-commits

# Derive a changeset from commits since a specific tag
cuenv changeset from-commits --since 0.49.0
```

### `cuenv release`

Release management operations. Release commands look for a `release` block in
the `env.cue` at the selected project root (package `cuenv`) and use it for tag
settings, package grouping, changelog settings, binary targets, and configured
release backends. When no `env.cue` release block is present, the commands keep
their built-in defaults.

#### `cuenv release prepare`

Prepare a release end-to-end: analyze commits, bump versions, generate the changelog, and open a pull request.

```bash
cuenv release prepare [OPTIONS]
```

**Options:**

- `-p, --path <PATH>`: Path to project root. Default: `.`
- `-s, --since <REF>`: Git tag or ref to analyze commits from.
- `--dry-run`: Preview changes without applying.
- `--branch <NAME>`: Branch name for the release. Default: `release/next`.
- `--no-pr`: Skip creating the pull request.

**Example:**

```bash
# Preview the full prepare flow
cuenv release prepare --dry-run

# Prepare on a custom branch without opening a PR
cuenv release prepare --branch release/2026-q2 --no-pr
```

#### `cuenv release version`

Calculate and apply version bumps from changesets.

```bash
cuenv release version [OPTIONS]
```

**Options:**

- `-p, --path <PATH>`: Path to project root. Default: `.`
- `--dry-run`: Show what would change without making changes.

#### `cuenv release publish`

Publish packages in topological order.

```bash
cuenv release publish [OPTIONS]
```

**Options:**

- `-p, --path <PATH>`: Path to project root. Default: `.`
- `--dry-run`: Show what would be published without publishing.

If `release.backends` is present, `publish` only uses configured package
publishing backends. The crates.io backend honors `tokenEnv`; the CUE registry
backend is recognized but non-dry-run publishing is not implemented yet.

#### `cuenv release binaries`

Build, package, and publish binary releases to configured backends.

```bash
cuenv release binaries [OPTIONS]
```

**Options:**

- `-p, --path <PATH>`: Path to project root. Default: `.`
- `--dry-run`: Preview without making changes.
- `--backend <LIST>`: Only run specific backend(s) (comma-separated).
- `--build-only`: Build only, don't publish.
- `--package-only`: Package only, don't publish (assumes binaries exist).
- `--publish-only`: Publish only (requires existing artifacts).
- `--target <LIST>`: Target platform(s) to build (comma-separated).
- `--version <VER>`: Version to release. Default: read from `Cargo.toml`.

**Example:**

```bash
# Preview the binary release flow
cuenv release binaries --dry-run

# Build for specific targets without publishing
cuenv release binaries --build-only --target x86_64-unknown-linux-gnu,aarch64-apple-darwin
```

:::note[Release backends are partial]
Config-driven release backends are not complete. Use `--dry-run` to verify behavior, and see [Schema status](/reference/schema/status/) for current support.
:::

### `cuenv web`

Reserved for a future web server for streaming cuenv events. The command currently exits with a configuration error instead of starting a placeholder server.

```bash
cuenv web [OPTIONS]
```

**Options:**

- `-p, --port <PORT>`: Port the future server would listen on. Default: `3000`
- `--host <HOST>`: Host the future server would bind to. Default: `127.0.0.1`

### `cuenv completions`

Generate shell completion setup instructions.

```bash
cuenv completions <SHELL>
```

**Arguments:**

- `<SHELL>`: Shell type (bash, zsh, fish, elvish, powershell).

**Example:**

```bash
# Show setup instructions for fish
cuenv completions fish
```

## Shell Completions

cuenv provides dynamic shell completions that include task names with descriptions directly from your CUE configuration. The completions are always up-to-date because they query your project's configuration at completion time.

### Setup

Add one of the following lines to your shell configuration file:

#### Bash

Add to `~/.bashrc`:

```bash
source <(COMPLETE=bash cuenv)
```

#### Zsh

Add to `~/.zshrc`:

```zsh
source <(COMPLETE=zsh cuenv)
```

#### Fish

Add to `~/.config/fish/config.fish`:

```fish
COMPLETE=fish cuenv | source
```

#### Elvish

Add to `~/.elvish/rc.elv`:

```text
eval (E:COMPLETE=elvish cuenv | slurp)
```

#### PowerShell

Add to your `$PROFILE`:

```powershell
$env:COMPLETE = "powershell"; cuenv | Out-String | Invoke-Expression; Remove-Item Env:\COMPLETE
```

### Features

Once completions are set up, you get:

- **Command completion**: Tab-complete all cuenv commands and options
- **Task name completion**: Tab-complete task names defined in your CUE configuration
- **Task descriptions**: See task descriptions in completion suggestions (shell dependent)

**Example:**

```bash
# Type 'cuenv task ' then press Tab to see available tasks
cuenv task <TAB>
# Shows: build  test  lint  deploy  ...

# Type 'cuenv -e ' then press Tab to see environment options
cuenv -e <TAB>
# Shows: development  staging  production  ...
```

:::note
We recommend re-sourcing completions on upgrade. The completion system calls the cuenv binary during completion, so the shell setup and binary should stay in sync.
:::

## Where to go next

- [Schema status](/reference/schema/status/) — the authoritative source for what is Stable, Partial, or schema-only.
- [Secrets Guide](/how-to/secrets/) — provider setup and runtime resolution.
- [Formatters Guide](/how-to/formatters/) — configuring `cuenv fmt`.
- [Ignore Files guide](/how-to/ignore-files/) — generating `.gitignore`/`.dockerignore` via `cuenv sync`.
