---
id: RFC-0007
title: Native Dagger v1 Runtime — CUE In, One Session, Engine-Owned Cache
status: Draft
decision_date: 2026-09-19
approvers:
  - TBD
related_features: []
---

## Summary

cuenv supports two execution homes: **local/host** (the default) and **Dagger** (explicit opt-in). Dagger is the native *container* engine, not the default for every task. Users opt in by writing `#DaggerRuntime` with an `image` or `from`. That is what names the base image; cuenv never invents one. Opted-in tasks compile into one Dagger v1 session so DagQL owns layer, exec, and cache-volume reuse. Host tasks keep `cuenv-cas`. `#ContainerRuntime` stays schema-only and is not a silent Dagger switch.

This RFC is the implementation plan. Schema status stays **Partial** until the phases below land. A Fable 5.1 adversarial UX review on 2026-09-19 produced the exec (`expect: ANY`), mount, secret, `--interactive`, and `#ContainerRuntime` union decisions below.

## Problem statement

Dagger is approaching 1.0. The engine now owns caching (DagQL replaced the BuildKit solver in 0.21; the Rust SDK has a `1.0.0-beta.14` line). cuenv already has a Dagger backend, but it is not native and it is not what the docs teach:

1. **The documented CUE does not execute.** Docs and `schema/runtime.cue` tell users to write `runtime: schema.#DaggerRuntime`. [`crates/dagger/src/lib.rs`](../../../../../crates/dagger/src/lib.rs) only reads legacy `task.dagger`. `#DaggerRuntime` is parsed and used as a cache-identity label, then ignored.
2. **Backend selection is global and legacy.** `TaskExecutor` picks one `TaskBackend` from `--backend` or `config.backend`. Closed `#Config` has no `backend` field, but [`examples/dagger-task/env.cue`](../../../../../examples/dagger-task/env.cue) still sets it. Mixed host/Dagger graphs cannot dispatch per task.
3. **Each task opens a new Dagger connection.** `connect_opts` runs inside every `execute`. That throws away DagQL session cache, makes `from:` chaining depend on in-process `ContainerId` luck, and is why `timeout` is rejected (cancelling the future would leak the engine exec).
4. **The SDK is a full minor behind the v1 line.** `cuenv-dagger` pins `dagger-sdk = "0.20.8"`. Current v1 crate is `1.0.0-beta.14` (2026-09-18); latest 0.21 line is `0.21.9`. `deny.toml` still ignores rustls advisories from the 0.20 SDK's reqwest 0.11 stack.
5. **Container results do not return to the host.** The backend mounts a host snapshot at `/workspace` and reads stdout/stderr. Declared `outputs` are not exported. `with_mounted_directory` is not a two-way bind.
6. **Image builds bypass Dagger.** `cuenv build` uses docker/buildx and Nix. `#ContainerImage` `.ref` / `.digest` resolution is incomplete. `#ContainerRuntime` is schema-only.
7. **cuenv-cas wraps Dagger tasks.** The executor looks up a Bazel-RE-shaped action cache *before* calling Dagger, so a hit never reaches the engine that now has the better cache.

The user-facing cost: "write the CUE we document, run `cuenv task`, get a hermetic cached container" does not work.

## Decision

CUE remains the authoring surface. cuenv is a Dagger v1 SDK *client*, not a module generator and not a second remote-exec implementation.

### Locked choices

| Choice | Decision |
| ------ | -------- |
| Default | Local/host execution. Nix, devenv, tools, and OCI runtimes still run on the host. |
| Dagger opt-in | A task runs in Dagger only when its effective runtime is `#DaggerRuntime` (or legacy `task.dagger`) **and** that spec has `image` or `from`. No image, no Dagger. |
| Not opt-in | Presence of Docker, `--backend dagger` without a Dagger spec or `--image`, or a project without `runtime: #DaggerRuntime`. |
| Authoring | `#DaggerRuntime` is the only supported Dagger surface. Remove `#ContainerRuntime` from the `#Runtime` union in Phase 1 so CUE rejects it at eval time with a pointer at `#DaggerRuntime`. Keep the type definition commented as deprecated. |
| Modules | Do not generate Dagger modules from CUE. Do not require users to write Go/TS/Python modules. |
| SDK | Pin `dagger-sdk` to `1.0.0-beta.14`. Engine/CLI must match. Bump to 1.0.0 stable in a follow-up when Dagger tags it. |
| Session | One Dagger session per graph that contains at least one opted-in Dagger task. Shared `DaggerBackend` holds the client and container-id map. |
| Dispatch | Per-task. Host and Dagger tasks may share one graph. |
| CLI | `--backend host` demotes opted-in Dagger tasks to local for debugging, and fails closed if any `#DaggerSecret.path` is set. `--image` implies Dagger and applies to the named tasks only; print the CUE line to persist. `--backend dagger` without `--image` or a CUE spec fails and points at `#DaggerRuntime`. `--interactive` (TTY) attaches `Container.terminal()` to the failed container, or prints a reproducible `dagger core … terminal` one-liner. |
| Defaults | Project `runtime: #DaggerRuntime & { image: ... }` is the opt-in default for tasks that inherit it. Do not add `config.backend` to closed `#Config`. |
| Legacy | Keep reading `task.dagger` as a shim onto `#DaggerRuntime`. Stop teaching `config.backend`. |
| Cache | Skip `cuenv-cas` action-cache wrap for Dagger-executed tasks. DagQL + `#DaggerCacheMount` are the cache. Host tasks keep `cuenv-cas`. |
| Outputs | After exec, export declared `outputs` from `/workspace` onto the host task workdir. |
| Timeout | Honour `task.timeout` by cancelling the in-flight session query; session teardown stops the engine exec. |
| Platform | `#DaggerRuntime.platform` is applied on `Container.from` / image build. |
| Images | `cuenv build` stays on docker/buildx (and Nix for `installable`) unless the caller passes `--backend dagger` or the image sets `builder: "dagger"`. A project that merely has some Dagger *tasks* does not change image builds. `#DaggerRuntime.image` accepts `string \| #ImageOutputRef` so a task can run in the image just built. |
| Mount | If the task declares `inputs`, mount only those (`Host.directory` include). Otherwise mount `task.dir` with excludes from `.gitignore` / `.daggerignore` plus `.git`. Emit `uploading N files (X MB)`. `/workspace` is the CUE module root so `dir: {from: "caller"\|"module"}` stays inside it. |
| Exec | `with_exec(argv, expect: ANY)` then read `exit_code` / `stdout` / `stderr`. Non-zero exits are `TaskResult`, not GraphQL errors. Transport/engine failures stay `Error::execution`. |
| Secrets | Secret-derived env keys and `#DaggerSecret` go through `set_secret` + `with_secret_variable` / `with_mounted_secret`. Never `with_env_variable` for secret values (those leak into DagQL cache keys and Cloud traces). Private base images use `with_registry_auth`. |
| Events | All Dagger user output goes through `cuenv_events`. No `print!` / `eprint!`. Result lines carry `engine=dagger` and the image. Engine progress is required (Rust SDK `Config.logger`). |

### Out of scope

- Generating or invoking Daggerverse modules from CUE
- Deleting `cuenv-cas` or the host hermetic cache ([ADR-0008](/decisions/adrs/adr-0008-hermetic-task-execution-cache/))
- Remote Bazel REAPI / bazel-remote / BuildBarn
- Building Nix `installable` images inside Dagger
- Dagger service bindings for `cuenv up` sidecars
- Calling published Daggerverse modules from CUE (a later RFC; this plan compiles CUE tasks to `Container.from` + `withExec`)
- Replacing Dagger's own TUI with a reimplementation. Phase 2 must still surface engine progress so `cuenv task` does not look hung.

## Native mapping

cuenv already has a task DAG. Dagger v1 is a DagQL DAG. The native integration is a compile, not a wrap-each-shell-in-a-new-engine.

```mermaid
flowchart TD
    cue[env.cue Project] --> eval[CUE eval]
    eval --> graph[Task graph]
    graph --> dispatch{effective runtime}
    dispatch -->|host Nix devenv tools oci or no dagger opt-in| host[HostBackend plus cuenv-cas]
    dispatch -->|explicit DaggerRuntime with image or from| session[Shared Dagger v1 session]
    session --> dagql[Container.from / load / dockerBuild]
    dagql --> exec[withExec plus cache mounts and secrets]
    exec --> export[Export declared outputs]
    exec --> chain[Remember ContainerId for from]
```

CUE field → Dagger v1 API:

| CUE | Dagger |
| --- | ------ |
| `runtime.image` | `client.container().from(image)` (with `platform` when set) |
| `runtime.from` | `client.load_container_from_id(id)` from the session map |
| `command` + `args` | `container.with_exec(argv, expect: ANY)` then `exit_code` / `stdout` / `stderr` |
| `script` / `scriptShell` | `command_spec()` (shell + script). Error if the image lacks that shell. |
| `task.env` | After output-ref and param resolution: plain values → `with_env_variable`; secret-derived keys → `set_secret`. |
| `runtime.cacheMounts[]` | `client.cache_volume(name)` + `with_mounted_cache` (`sharing?: "shared"\|"private"\|"locked"`) |
| `runtime.secrets[]` | `client.set_secret` + `with_secret_variable` / `with_mounted_secret` |
| project `env` (non-secret) | `with_env_variable` |
| `timeout` | cancel the session query; treat as a hard timeout (not retried) |
| `outputs` | `container.directory("/workspace").file(path).export(host)` |
| `images.*.context` | host directory + `docker_build` / `publish` |
| `images.*.ref` / `.digest` | publish/load result written back as output refs |

Users keep writing the CUE already shown in [Dagger Runtime](/explanation/dagger-backend/). That page becomes true.

## Ergonomic authoring

Local is the zero-config path: a task with a `command` and no Dagger runtime runs on the host.

The smallest Dagger opt-in is one field that names the image:

```cue
tasks: {
	test: schema.#Task & {
		command: "pytest"
		runtime: schema.#DaggerRuntime & {image: "python:3.12-slim"}
	}
}
```

Project-level opt-in applies to tasks that inherit it. The project still has to name the image:

```cue
runtime: schema.#DaggerRuntime & {image: "alpine:3.20"}

tasks: {
	hello: schema.#Task & {command: "hostname"}
	py: schema.#Task & {
		command: "python"
		args: ["-c", "print(1)"]
		runtime: schema.#DaggerRuntime & {image: "python:3.12-slim"}
	}
}
```

`cuenv task test` is enough **after** that opt-in. No `config.backend`. `#ContainerRuntime` is not an opt-in.

`--backend host` demotes an opted-in Dagger task to local for debugging. `--backend dagger` never invents a base image. Discovery one-shot:

```bash
cuenv task test --backend dagger --image python:3.12-slim
```

`--image` is the opt-in for the named tasks and implies Dagger (no `--backend dagger` required). Print the CUE line to persist. `--backend dagger` alone still fails and points at the runtime form.

Phase 1 drops `#ContainerRuntime` from the `#Runtime` union so CUE rejects it at eval time with that same pointer. Do not keep a type whose only runtime behaviour is an error.

## Effective runtime

Resolve once per task, before backend dispatch:

1. `task.runtime` if it is `#DaggerRuntime` with `image` or `from`
2. else legacy `task.dagger` with `image` or `from` (shim onto `#DaggerRuntime`)
3. else project `runtime` if it is `#DaggerRuntime` with `image` or `from`
4. else local/host (Nix/devenv/tools/oci env acquisition stays on the host)

`#DaggerRuntime` without `image` or `from` is a configuration error, not a host fallback and not a guessed `alpine`. `#ContainerRuntime` is not in the union.

`--backend host` skips steps 1–3 and runs locally. `--backend dagger` uses `--image` if given, otherwise requires steps 1–3; it does not invent an image.

`TaskExecutor` holds both backends. The Dagger session is created only when at least one selected task opted in, and is shared across graph clones so `from:` and DagQL reuse work.

## Session, timeout, outputs

Today `DaggerBackend::execute` calls `connect_opts` per task, then `print!`s stdout. The v1 backend:

1. Connects once when the executor starts a graph that needs Dagger.
2. Reuses that client for every Dagger task in the graph.
3. Stores `ContainerId` by task name for `from:`.
4. On `timeout`, cancels the in-flight query and ends the attempt as a hard timeout (same policy as host: not retried).
5. On success, exports each declared output from `/workspace` into the host workdir used by captures, downstream `inputs`, and (host-only) `cuenv-cas`.
6. Emits stdout/stderr through `cuenv_events`, never `print!`.
7. Drops the session when the graph finishes so engine teardown cannot leak.

Chaining and workdir rules:

- `from:` is a CUE task reference (like `dependsOn`), implies that edge, and is graph-scoped. IDs live in the session, not across `cuenv` processes. Host → Dagger `from:` is a configuration error.
- Honour `task.dir` as `with_workdir` (resolved under `/workspace`).
- Mount per the Mount locked choice. Mixed graphs share artifacts only through declared `outputs` / `inputs`.
- `hermetic: false` on a Dagger task is a configuration error.
- First connect failure when no engine is present must say how to install/start Dagger and which engine version this SDK pin expects. Version skew is a configuration error, not a GraphQL dump. Honour `_EXPERIMENTAL_DAGGER_CLI_BIN`. When the `dagger-backend` feature is compiled out, a `#DaggerRuntime` task fails; it does not fall back to host.
- Engine progress through `cuenv_events` is required (`Config.logger`).
- Timeout via query cancel is the only per-task mechanism Dagger exposes (`withExec` has no deadline). Word it honestly: the engine may keep the exec until session teardown. Engine-gated test must prove the exec dies on cancel.
- `--interactive` attaches to the failed container. `--backend host` demote prints that cache mounts are ignored and refuses `#DaggerSecret.path`.

## Cache split

Two caches, one job each:

- **Host tasks:** `cuenv-cas` action cache, per [ADR-0008](/decisions/adrs/adr-0008-hermetic-task-execution-cache/). Local, input-addressed, stays.
- **Dagger tasks:** do not consult or record `cuenv-cas`. The engine's DagQL cache plus named `cacheMounts` volumes are the hit path. Result lines and `--dry-run` carry `engine=dagger` and the image. `task.cache.mode` on a Dagger task emits `task.cache.skipped reason=engine=dagger`. `--show-cache-path` explains rather than printing a CAS path. Rename the runtime field from `cache` to `cacheMounts` (with `sharing`) so it does not collide with `#Task.cache`. We do not invent pip/cargo volumes.

This is how cuenv avoids owning a Bazel-style CAS for containerized CI. It is not a deletion of `cuenv-cas`.

## Schema changes

All schema edits land with Phase 1 so the documented form is complete before the SDK bump.

In [`schema/runtime.cue`](../../../../../schema/runtime.cue):

- Add `platform?: string` to `#DaggerRuntime`.
- Rename `cache` to `cacheMounts` and add `sharing?: "shared" | "private" | "locked"`.
- Accept `image?: string | #ImageOutputRef`.
- Add reserved `module?` / `function?` fields (schema-only, unused) for a later module-call RFC.
- Move `#DaggerSecret` and `#DaggerCacheMount` next to `#DaggerRuntime`.
- Drop `#ContainerRuntime` from the `#Runtime` union.

In [`schema/tasks.cue`](../../../../../schema/tasks.cue):

- Keep deprecated `dagger?: #DaggerConfig` as a read-compat alias of the same fields.
- `#DaggerConfig` stays `legacy`. `#DaggerSecret` / `#DaggerCacheMount` leave the legacy column once they live on the runtime.

Do not add `backend` to [`schema/config.cue`](../../../../../schema/config.cue).

`#ContainerImage` gains optional `builder?: "docker" | "dagger"`. Dockerfile CUE stays `context` / `dockerfile` / `target` / `buildArgs` / `tags` / `registry` / `platform`. Execution stays docker/buildx unless `builder: "dagger"` or `cuenv build --backend dagger`.

## Implementation phases

### Phase 1 — Documented CUE runs

Close the honesty gap without waiting on the SDK bump if the current 0.20 client still compiles.

- Resolve the effective Dagger spec in `cuenv-task-exec` / `cuenv-dagger` (`task.runtime`, then `task.dagger`, then project `runtime`). Require `image` or `from`.
- Per-task dispatch: local vs opted-in Dagger on one graph; share one `DaggerBackend` `Arc` only when at least one task opted in.
- Drop `#ContainerRuntime` from the union. `--backend host` demotes (with path-secret / cache-mount rules). `--image` implies Dagger; `--backend dagger` alone still requires a spec.
- Add `contrib/contributors/dagger.cue` (`when: runtimeType: ["dagger"]`) that installs the SDK-pinned engine/CLI and passes `DAGGER_CLOUD_TOKEN` when set.
- Feature-off: `#DaggerRuntime` fails, never host fallback.
- Stop requiring `config.backend`. Delete it from the example.
- Rewrite [`examples/dagger-task/env.cue`](../../../../../examples/dagger-task/env.cue) to `#DaggerRuntime`.
- Replace `print!` / `eprint!` with `cuenv_events`.
- Unit-test the resolution table (runtime wins, legacy shim, project default, host default, `--backend host` demote, `--backend dagger` without a spec, missing image/`from`).
- Update the matrix notes: `#DaggerRuntime` still `partial` until Phase 2 session + export land; example citation becomes accurate.

Focused gate: `cuenv fmt --fix`, `git diff --check`, `cuenv exec -- cargo test -p cuenv-dagger --lib`, `cuenv exec -- cargo test -p cuenv-task-exec --lib`, CLI smoke `cuenv task --help`, `cuenv task ci.schema-docs-check`. Full flake before review (cross-crate runtime + schema/CLI).

### Phase 2 — Dagger v1 session

- Bump `dagger-sdk` to `1.0.0-beta.14` in [`crates/dagger/Cargo.toml`](../../../../../crates/dagger/Cargo.toml). Adapt `connect_opts` / `Config` builder / generated IDs as the crate requires.
- Hold one session for the graph. Drop per-task `connect_opts`.
- Apply `platform`.
- `with_exec(..., expect: ANY)`; non-zero exit is a `TaskResult`. Prove this with an integration test (`pytest` exit 1).
- Honour `timeout` via query cancel + session teardown; prove the exec dies.
- Export declared outputs from `/workspace`.
- Skip `cuenv-cas` lookup/record when the dispatched backend is Dagger; label results `engine=dagger`.
- Mount only declared inputs (or gitignore-excluded `dir`). Emit upload size.
- Map `script` and `task.env`; secret env keys through `set_secret`.
- `--interactive` on TTY; engine `Config.logger` progress.
- Remove the dagger-sdk rustls advisory ignores in `deny.toml` if the 1.0 crate left reqwest 0.11.
- Add engine-gated integration tests that skip cleanly when no Dagger engine is present.
- Surface engine progress and a first-run "Dagger engine missing / version mismatch" error.
- Honour `task.dir`; reject illegal `from:` (host predecessor or missing from this graph).

Focused gate: crate tests + clippy for `cuenv-dagger` / `cuenv-task-exec` / `cuenv`. Full flake before review (production lockfile + cross-crate runtime).

### Phase 3 — Dockerfile images through Dagger

- Build `#ContainerImage` Dockerfile images with Dagger `dockerBuild` only when `builder: "dagger"` or `cuenv build --backend dagger`. Otherwise keep docker/buildx.
- Allow `#DaggerRuntime.image: images.app.ref`.
- Pin `#DaggerRuntime.image` tags to digests via `cuenv sync lock` when that path exists.
- Push with `publish` when `registry` is set; write `.ref` and `.digest`.
- Leave Nix `installable` images on the current nix+docker path.
- `#ContainerRuntime` is already out of the union from Phase 1.
- Promote matrix rows that the phases actually finish (`#DaggerRuntime` toward `implemented` only if timeout, export, session, and the explicit runtime form all work; `#ContainerImage` notes lose "Dagger pending" for Dockerfile).

Focused gate: `cuenv build` smoke on `examples/container-image`, schema-docs-check, crate tests. Full flake before review.

## Documentation and skills

Every phase updates:

- [Dagger Runtime](/explanation/dagger-backend/) — remove the "example is legacy" discrepancy once Phase 1 migrates it; keep Partial until Phase 2.
- [Runtimes](/how-to/runtimes/), [Run tasks](/how-to/run-tasks/), [Container images](/how-to/container-images/)
- [`docs/design/specs/schema-coverage-matrix.md`](../../../../../docs/design/specs/schema-coverage-matrix.md)
- [`.agents/skills/cuenv-services-images-runtime/SKILL.md`](../../../../../.agents/skills/cuenv-services-images-runtime/SKILL.md) and the schema-first adversarial prompts
- [Roadmap](/explanation/roadmap/) — Dagger v1 native runtime is Next; remote Bazel cache is not the container story

Run `cuenv task ci.schema-docs-check` on those edits.

After Phase 2, write a short ADR that records the cache split (host `cuenv-cas`, Dagger DagQL) next to ADR-0008. Do not rewrite ADR-0008 until that ADR exists.

## Golden path

Typical app: lint on the host, tests in Dagger, image build only when opted in.

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "app"

tasks: {
	lint: schema.#Task & {
		command: "cargo"
		args: ["clippy", "--", "-D", "warnings"]
	}

	test: schema.#Task & {
		command: "cargo"
		args: ["test"]
		inputs: ["Cargo.toml", "Cargo.lock", "src/**", "tests/**"]
		timeout: "15m"
		runtime: {
			type:  "dagger"
			image: "rust:1.85-slim"
			cacheMounts: [{path: "/usr/local/cargo/registry", name: "cargo-registry", sharing: "locked"}]
		}
	}
}

images: {
	app: schema.#ContainerImage & {
		context: "."
		tags: ["latest"]
	}
}
```

```bash
cuenv task lint                 # host
cuenv task test                 # Dagger; uploads only declared inputs
cuenv task test --interactive   # shell in the failed container
cuenv task test --image rust:1.88-slim   # one-shot; implies Dagger; prints CUE to persist
cuenv task test --backend host  # demote; cache mounts ignored
cuenv build app                 # docker/buildx unless builder: "dagger"
```

## Consequences

- Local execution stays the default. Dagger runs only when CUE names `#DaggerRuntime` and an `image` or `from`.
- Users write the CUE we already document. After that opt-in, `cuenv task` just works.
- Container cache quality tracks Dagger 1.0 instead of a cuenv-owned REAPI.
- Host hermetic cache remains for non-container tasks.
- `cuenv-dagger` stays a small SDK client. We do not become a Dagger module SDK.
- 1.0-beta crates can churn; Phase 2 isolates the bump. A stable `1.0.0` pin is a later one-line follow-up.
- Mixed host/Dagger graphs become a supported shape, which they are not today.
