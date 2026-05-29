---
title: Choosing a runtime
description: Declare where each task runs — host, Nix shell, devenv, OCI binaries, or Dagger — with one CUE field, and know each variant's honest support status.
---

A task always runs *somewhere*. By default that somewhere is your host shell, with whatever happens to be on `PATH`. cuenv lets you make that choice explicit and reproducible: declare a **runtime** once at the project level, and every task inherits the same hermetic environment — a Nix devShell, a devenv shell, a set of binaries extracted from OCI images, or a Dagger container — without anyone needing to remember `nix develop` first.

One typed field replaces "works on my machine":

```cue
runtime: schema.#NixRuntime
```

Now `cuenv task build`, `cuenv exec -- cargo test`, and CI all see the same toolchain. And when one task needs something different — a heavier container, a pinned binary — you override it on that single task.

:::caution[Status varies by variant]
The `#Runtime` union has six members, and they are **not** at the same maturity. Nix, devenv, and tools runtimes are solid. OCI and Dagger are partial. The container runtime is schema-only and does nothing today. Read the [status table](#status-by-variant) below before you build on a variant, and cross-check the authoritative [schema status](/reference/schema/status/) page.
:::

## The runtime union

`#Runtime` is a CUE union — exactly one of these shapes (from `schema/runtime.cue`):

```cue
#Runtime: #NixRuntime | #DevenvRuntime | #ContainerRuntime | #DaggerRuntime | #OCIRuntime | #ToolsRuntime
```

You select a variant by assigning it directly. The `type` field discriminates which one you mean:

| Variant            | `type`        | What it gives a task                                          |
| ------------------ | ------------- | ------------------------------------------------------------- |
| `#NixRuntime`      | `"nix"`       | A Nix flake devShell environment                              |
| `#DevenvRuntime`   | `"devenv"`    | A [devenv](https://devenv.sh) shell environment               |
| `#ToolsRuntime`    | (tools)       | Hermetic, version-pinned binaries from GitHub/OCI/Nix/Rustup  |
| `#OCIRuntime`      | `"oci"`       | Specific binaries extracted from whole OCI images onto `PATH` |
| `#DaggerRuntime`   | `"dagger"`    | Containerized execution with chaining, cache volumes, secrets |
| `#ContainerRuntime`| `"container"` | (schema-only — see below)                                     |

## Project default vs per-task override

The runtime model is **declare once, override where needed**. Set `runtime` at the project level and it becomes the default for every task. Override it on an individual `#Task` when one task needs a different home.

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
    name: "api"

    // Project-level default: every task runs in the Nix devShell.
    runtime: schema.#NixRuntime

    tasks: {
        // Inherits the project Nix runtime.
        build: schema.#Task & {
            command: "cargo"
            args: ["build", "--release"]
        }

        // Overrides the default for this one task.
        lint: schema.#Task & {
            command: "golangci-lint"
            args: ["run"]
            runtime: schema.#NixRuntime & {output: "devShells.x86_64-linux.ci"}
        }
    }
}
```

This is the same pattern across every variant: the project `runtime` is the baseline, and `#Task.runtime` wins for that task. Keep the default broad and reserve overrides for the exceptions.

## Status by variant

cuenv is honest about what works. The matrix in [`docs/design/specs/schema-coverage-matrix.md`](/reference/schema/status/) is authoritative; this is the runtime slice of it:

| Variant             | Status          | Use it?                                                                                  |
| ------------------- | --------------- | ---------------------------------------------------------------------------------------- |
| `#NixRuntime`       | **Stable**      | Yes. Preferred for reproducible toolchains. See [Nix integration](/how-to/nix/).         |
| `#DevenvRuntime`    | **Stable**      | Yes, if your project already uses devenv. See [below](#devenv-runtime).                  |
| `#ToolsRuntime`     | **Stable**      | Yes. Best for pinning individual CLI tools. See [Tools](/how-to/tools/).                  |
| `#OCIRuntime`       | **Partial**     | Yes, with caveats — `sync lock` + the `#OCIActivate` hook. See [below](#oci-runtime).    |
| `#DaggerRuntime`    | **Partial**     | Usable; task-level `dagger:` is deprecated — prefer the runtime form. See [below](#dagger-runtime). |
| `#ContainerRuntime` | **schema-only** | No. Returns no container environment today. Do not rely on it. See [below](#container-runtime). |

How each variant supplies an environment differs under the hood. Nix and devenv runtimes are resolved by `cuenv` itself (it runs `nix print-dev-env` / `devenv print-dev-env` and sources the result, per `crates/core/src/runtime.rs`). OCI binaries are extracted by the `#OCIActivate` hook. Dagger runs tasks inside the Dagger engine.

## OCI runtime

`#OCIRuntime` (Partial) fetches **specific binaries out of whole OCI images** and puts them on `PATH`. This is the working path for "I want the `nginx` binary from `nginx:1.25-alpine`, hermetically, content-addressed." It is a three-step flow.

:::note[OCI runtime vs the #Oci tool source]
Do not confuse `#OCIRuntime` with the `#Oci` *tool source* used inside `#ToolsRuntime`. The `#Oci` tool source extracts **one** binary at a given `path` and is a member of the [Tools](/how-to/tools/) system. `#OCIRuntime` is a whole-runtime declaration that can pull **multiple** binaries from **multiple** images and is activated by its own hook. Reach for the tool source when you want a single pinned CLI; reach for `#OCIRuntime` when binary extraction *is* the runtime.
:::

### 1. Declare the runtime

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
    name: "edge"

    runtime: schema.#OCIRuntime & {
        // Platforms to resolve and lock.
        platforms: ["darwin-arm64", "linux-x86_64"]
        images: [
            // Extract one binary; PATH name defaults to the filename ("nginx").
            {image: "nginx:1.25-alpine", extract: [{path: "/usr/sbin/nginx"}]},
            // Rename the extracted binary in PATH via `as`.
            {image: "busybox:latest", extract: [{path: "/bin/sh", as: "busybox-sh"}]},
        ]
    }
}
```

Each `images` entry takes a `#OCIImage` with a required `image` reference and an `extract` list. Each `#OCIExtract` needs a `path` (the binary inside the image); `as` renames it in `PATH` and defaults to the filename from `path`.

### 2. Resolve digests and extract paths into the lockfile

```bash
cuenv sync lock
```

This resolves each image to a content-addressed digest and records the per-image `extract` paths for every configured platform into `cuenv.lock`. Commit the lockfile so every machine and CI run resolves the same bytes. See [Lockfiles](/how-to/lockfiles/) for the full lock model.

### 3. Activate binaries with the #OCIActivate hook

The lockfile alone does not change `PATH`. Add the pre-configured `#OCIActivate` hook to `onEnter`:

```cue
hooks: {
    onEnter: {
        oci: schema.#OCIActivate
    }
}
```

`#OCIActivate` is an `#ExecHook` that runs `cuenv runtime oci activate`, which (per the header comment in `schema/runtime.cue`):

1. Reads `cuenv.lock` to find artifacts for the current platform.
2. Pulls and extracts the binaries (skipping anything already cached).
3. Emits `export PATH=...` so the extracted binaries lead your `PATH`.

You can also run the activation directly in a script:

```bash
eval "$(cuenv runtime oci activate)"
```

:::caution[Partial — end-to-end fixture pending]
The OCI runtime is **partial**. `cuenv sync lock` resolves digests and per-image `extract` paths into `cuenv.lock`, and `#OCIActivate` extracts those binaries and prepends them to `PATH`. A full end-to-end fixture is still pending, so validate the behaviour for your images before relying on it in production. Track the [schema status](/reference/schema/status/) page for updates.
:::

## devenv runtime

`#DevenvRuntime` (Stable) activates a [devenv](https://devenv.sh) shell. cuenv runs `devenv print-dev-env` and sources the result; if `devenv` is not on `PATH`, cuenv installs it via `nix profile install nixpkgs#devenv` (see `crates/core/src/runtime.rs`).

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
    name: "web"

    // path defaults to "." (the project directory).
    runtime: schema.#DevenvRuntime
}
```

To point at a devenv config in a subdirectory, set `path`:

```cue
runtime: schema.#DevenvRuntime & {path: "./infra"}
```

:::note[Runtime vs standalone hook]
`#DevenvRuntime` is the *runtime* form. There is a separate standalone `#Devenv` hook for projects that prefer to wire devenv as an explicit hook. See [Nix integration](/how-to/nix/) for the hook-driven approach, including the `#NixFlake` hook that is the analogue for plain Nix flakes.
:::

## Dagger runtime

`#DaggerRuntime` (Partial) runs tasks inside the Dagger engine, with container chaining, cache volumes, and securely mounted secrets. Use it when a plain devShell is not enough — for example, building inside a pinned base image and continuing from a previous task's container state.

```cue
build: schema.#Task & {
    command: "sh"
    args: ["-c", "apk add --no-cache curl jq && echo ready"]
    runtime: schema.#DaggerRuntime & {image: "alpine:latest"}
}
```

`#DaggerRuntime` supports `image` (base image), `from` (continue from another task's container), `secrets`, and `cache` volumes.

:::caution[Task-level `dagger:` is deprecated]
Older configurations attach a `dagger:` block directly to a `#Task` (as in `examples/dagger-task/env.cue`). That task-level form still works but is **deprecated** — prefer the `runtime: schema.#DaggerRuntime` form going forward. For the full picture of how the Dagger backend executes, see the [Dagger backend explanation](/explanation/dagger-backend/).
:::

## Container runtime

`#ContainerRuntime` is **schema-only**. The shape exists in `schema/runtime.cue` for a future "just run this task in this image" use case, but runtime environment acquisition returns no container environment today.

```cue
// schema-only: this does NOT run your task in a container yet.
runtime: schema.#ContainerRuntime & {image: "node:20"}
```

Do not recommend or depend on `#ContainerRuntime`. If you need containerized execution today, use [`#DaggerRuntime`](#dagger-runtime) instead.

## Where to go next

<div>

- [Nix integration](/how-to/nix/) — the Stable `#NixRuntime` and the `#NixFlake` / `#Devenv` hooks in depth.
- [Tools](/how-to/tools/) — the Stable `#ToolsRuntime` for pinning individual CLI tools, including the `#Oci` single-tool source.
- [Lockfiles](/how-to/lockfiles/) — how `cuenv sync lock` and `cuenv.lock` underpin OCI and tools runtimes.
- [Dagger backend](/explanation/dagger-backend/) — how containerized task execution works.
- [Schema status](/reference/schema/status/) — the authoritative, per-definition support matrix.

</div>
