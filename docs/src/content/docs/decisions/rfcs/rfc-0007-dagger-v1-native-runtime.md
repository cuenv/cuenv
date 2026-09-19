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

This RFC is the implementation plan. Schema status stays **Partial** until the phases below land.

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
| Not opt-in | `#ContainerRuntime`, presence of Docker, `--backend dagger` without a Dagger spec, or a project without `runtime: #DaggerRuntime`. |
| Authoring | `#DaggerRuntime` is the only supported Dagger surface. `#ContainerRuntime` stays schema-only. |
| Modules | Do not generate Dagger modules from CUE. Do not require users to write Go/TS/Python modules. |
| SDK | Pin `dagger-sdk` to `1.0.0-beta.14`. Engine/CLI must match. Bump to 1.0.0 stable in a follow-up when Dagger tags it. |
| Session | One Dagger session per graph that contains at least one opted-in Dagger task. Shared `DaggerBackend` holds the client and container-id map. |
| Dispatch | Per-task. Host and Dagger tasks may share one graph. |
| CLI | `--backend host` demotes opted-in Dagger tasks to local for debugging. `--backend dagger` does not invent an image: it fails unless every selected task already has an explicit Dagger spec with `image` or `from`. |
| Defaults | Project `runtime: #DaggerRuntime & { image: ... }` is the opt-in default for tasks that inherit it. Do not add `config.backend` to closed `#Config`. |
| Legacy | Keep reading `task.dagger` as a shim onto `#DaggerRuntime`. Stop teaching `config.backend`. |
| Cache | Skip `cuenv-cas` action-cache wrap for Dagger-executed tasks. DagQL + `#DaggerCacheMount` are the cache. Host tasks keep `cuenv-cas`. |
| Outputs | After exec, export declared `outputs` from `/workspace` onto the host task workdir. |
| Timeout | Honour `task.timeout` by cancelling the in-flight session query; session teardown stops the engine exec. |
| Platform | `#DaggerRuntime.platform` is applied on `Container.from` / image build. |
| Images | Dockerfile `#ContainerImage` (`context`) builds through Dagger `dockerBuild` / `publish`. Nix `installable` images stay on the existing nix+docker path. |
| Secrets | Unchanged shape: cuenv resolvers → `client.set_secret` → env or file mount. `#DaggerSecret` / `#DaggerCacheMount` are first-class runtime types, not legacy. |
| Events | All Dagger user output goes through `cuenv_events`. No `print!` / `eprint!`. |

### Out of scope

- Generating or invoking Daggerverse modules from CUE
- Deleting `cuenv-cas` or the host hermetic cache ([ADR-0008](/decisions/adrs/adr-0008-hermetic-task-execution-cache/))
- Remote Bazel REAPI / bazel-remote / BuildBarn
- Building Nix `installable` images inside Dagger
- Dagger service bindings for `cuenv up` sidecars
- Shipping Dagger OTLP into the TUI (may follow once the session exists)

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
| `command` + `args` | `container.with_exec(argv)` |
| `runtime.cache[]` | `client.cache_volume(name)` + `with_mounted_cache` |
| `runtime.secrets[]` | `client.set_secret` + `with_secret_variable` / `with_mounted_secret` |
| `env` | `with_env_variable` (resolved cuenv env, redacted) |
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

`--backend host` demotes an opted-in Dagger task to local for debugging. `--backend dagger` never supplies a base image: if a selected task has no `#DaggerRuntime`/`task.dagger` with `image` or `from`, fail and point at the runtime form.

## Effective runtime

Resolve once per task, before backend dispatch:

1. `task.runtime` if it is `#DaggerRuntime` with `image` or `from`
2. else legacy `task.dagger` with `image` or `from` (shim onto `#DaggerRuntime`)
3. else project `runtime` if it is `#DaggerRuntime` with `image` or `from`
4. else local/host (Nix/devenv/tools/oci env acquisition stays on the host)

`#DaggerRuntime` without `image` or `from` is a configuration error, not a host fallback and not a guessed `alpine`. `#ContainerRuntime` never enters this table.

`--backend host` skips steps 1–3 and runs locally. `--backend dagger` requires steps 1–3 to produce a spec; it does not invent one.

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

Chaining rule stays: do not remount `/workspace` when continuing `from:` a prior container. Fresh images still mount the host project (or the hermetic workdir) at `/workspace`.

## Cache split

Two caches, one job each:

- **Host tasks:** `cuenv-cas` action cache, per [ADR-0008](/decisions/adrs/adr-0008-hermetic-task-execution-cache/). Local, input-addressed, stays.
- **Dagger tasks:** do not consult or record `cuenv-cas`. The engine's DagQL cache plus named `#DaggerCacheMount` volumes are the hit path. cuenv still emits `task_cache_*` events from Dagger when we can distinguish a cached exec; until the SDK exposes that cleanly, skip the wrap and let the engine be silent-fast.

This is how cuenv avoids owning a Bazel-style CAS for containerized CI. It is not a deletion of `cuenv-cas`.

## Schema changes

All schema edits land with Phase 1 so the documented form is complete before the SDK bump.

In [`schema/runtime.cue`](../../../../../schema/runtime.cue):

- Add `platform?: string` to `#DaggerRuntime` (OCI platform, e.g. `"linux/amd64"`).
- Move `#DaggerSecret` and `#DaggerCacheMount` next to `#DaggerRuntime` (or `#Secret` reuse for the resolver field) and treat them as implemented runtime types.

In [`schema/tasks.cue`](../../../../../schema/tasks.cue):

- Keep deprecated `dagger?: #DaggerConfig` as a read-compat alias of the same fields.
- `#DaggerConfig` stays `legacy`. `#DaggerSecret` / `#DaggerCacheMount` leave the legacy column once they live on the runtime.

Do not add `backend` to [`schema/config.cue`](../../../../../schema/config.cue).

`#ContainerImage` gains no new fields. Dockerfile execution moves from docker/buildx to Dagger; the CUE stays `context` / `dockerfile` / `target` / `buildArgs` / `tags` / `registry` / `platform`.

## Implementation phases

### Phase 1 — Documented CUE runs

Close the honesty gap without waiting on the SDK bump if the current 0.20 client still compiles.

- Resolve the effective Dagger spec in `cuenv-task-exec` / `cuenv-dagger` (`task.runtime`, then `task.dagger`, then project `runtime`). Require `image` or `from`.
- Per-task dispatch: local vs opted-in Dagger on one graph; share one `DaggerBackend` `Arc` only when at least one task opted in.
- Do not treat `#ContainerRuntime` as Dagger. `--backend host` demotes; `--backend dagger` refuses tasks without an explicit Dagger spec.
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
- Honour `timeout` via query cancel + session teardown.
- Export declared outputs from `/workspace`.
- Skip `cuenv-cas` lookup/record when the dispatched backend is Dagger.
- Remove the dagger-sdk rustls advisory ignores in `deny.toml` if the 1.0 crate left reqwest 0.11.
- Add engine-gated integration tests that skip cleanly when no Dagger engine is present.

Focused gate: crate tests + clippy for `cuenv-dagger` / `cuenv-task-exec` / `cuenv`. Full flake before review (production lockfile + cross-crate runtime).

### Phase 3 — Dockerfile images through Dagger

- Build `#ContainerImage` Dockerfile images with Dagger `dockerBuild` when the user asks `cuenv build` (that command is the opt-in for image builds). Push with `publish` when `registry` is set; write `.ref` and `.digest`.
- Leave Nix `installable` images on the current nix+docker path.
- Leave `#ContainerRuntime` schema-only. A later RFC can decide whether it becomes a second opt-in or is removed.
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

## Consequences

- Local execution stays the default. Dagger runs only when CUE names `#DaggerRuntime` and an `image` or `from`.
- Users write the CUE we already document. After that opt-in, `cuenv task` just works.
- Container cache quality tracks Dagger 1.0 instead of a cuenv-owned REAPI.
- Host hermetic cache remains for non-container tasks.
- `cuenv-dagger` stays a small SDK client. We do not become a Dagger module SDK.
- 1.0-beta crates can churn; Phase 2 isolates the bump. A stable `1.0.0` pin is a later one-line follow-up.
- Mixed host/Dagger graphs become a supported shape, which they are not today.
