---
title: Build container images
description: Declare container images as first-class DAG artifacts in CUE and build them — Dockerfile or Nix-native — with a single cuenv build.
---

Your images already live downstream of everything else you have typed in CUE: the code generation that produces sources, the tags your release process expects, the registry your cluster pulls from. So why are they still defined in a separate `Makefile` target, a hand-written `docker build` line, or a bespoke CI step that drifts away from the rest of your config?

With cuenv, a container image is a **first-class artifact in the same task DAG** as everything else. You declare it once next to your tasks, point it at a Dockerfile *or* a Nix flake output, and `cuenv build` discovers it, plans it, and builds it — locally with the Docker CLI or pushed to a registry with `docker buildx`. The image can depend on a task (run codegen first), carry tags and labels, and target multiple platforms. One typed definition, one command.

:::note[Status: partial]
Container images are a **partial** feature. `cuenv build` lists image definitions and builds selected images with the local Docker CLI today; registry builds push with `docker buildx`, and Nix images build with `nix build` then `docker load`. The gaps are real and called out below: downstream image **output-reference** resolution (`.ref` / `.digest`) is incomplete, multi-arch Nix images are unsupported, and image-backed service `dependsOn` fails fast. Always check [Schema status](/reference/schema/status/) before relying on a capability.
:::

## Two images, derived from a real example

Everything below comes from the runnable [`examples/container-image`](https://github.com/cuenv/cuenv/tree/main/examples/container-image) example. Images live in an `images:` block alongside `tasks:`, and each one is a `schema.#ContainerImage`. An image is built from **exactly one** source: a Dockerfile (`context`) *or* a Nix flake output (`installable`).

```cue
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "container-image-example"

tasks: {
    codegen: schema.#Task & {
        command: "echo"
        args: ["generating proto files"]
        hermetic: false
    }
}

images: {
    // 1) Dockerfile image: built from a build context, after codegen runs.
    api: schema.#ContainerImage & {
        context:    "."
        dockerfile: "Dockerfile"
        tags: ["latest", "v1.0.0"]
        dependsOn: [tasks.codegen]
        inputs: ["src/**", "Dockerfile"]
        description: "API server container image"
    }

    // 2) Nix-native image: built from a flake output and pushed to a registry.
    tools: schema.#ContainerImage & {
        installable: ".#images.tools"
        tags: ["latest"]
        registry:   "ghcr.io/myorg"
        repository: "myorg/tools"
        description: "Nix-built tools image"
    }
}
```

### The Dockerfile image (`api`)

- `context` is the build context directory; `dockerfile` is its path relative to that context (defaults to `Dockerfile`).
- `tags` are the tags applied to the built image. With no `registry`, the image is named `api:latest` and `api:v1.0.0` (the image key is the default repository).
- `dependsOn: [tasks.codegen]` places the image **downstream of a task** in the DAG — the `codegen` task runs to completion before the image builds.
- `inputs` declare the files that feed cache-key computation, just like task inputs.
- A multi-stage build can target a stage with `target: "worker"`.

### The Nix-native image (`tools`)

- `installable` points at a Nix flake output (for example `.#images.tools`) that produces an image archive — typically via `dockerTools.buildLayeredImage`.
- `registry` + `repository` give the published reference. With `registry: "ghcr.io/myorg"` and `repository: "myorg/tools"`, the pushed reference is `ghcr.io/myorg/myorg/tools:latest`.
- Set `installable` **or** `context`, never both. `cuenv build` rejects an image that sets both, and one that sets neither.

## Build them

`cuenv build` is the single entry point. Run it from a checkout of the repository to try the example.

### List every configured image

Run `cuenv build` with no names and no `--label` to print the configured images instead of building anything:

```bash
cuenv build --path examples/container-image --package examples
```

```text
Available images:

  api [latest, v1.0.0]  API server container image
  tools [latest]  Nix-built tools image
  worker [latest]  Background worker image
```

### Build one image by name

```bash
cuenv build api --path examples/container-image --package examples
```

cuenv prints the plan, then the invocation it runs:

```text
cuenv build: api (context: ., dockerfile: Dockerfile, registry: local, platform: native)
cuenv build: running docker build -f ./Dockerfile -t api:latest -t api:v1.0.0 .
```

### Build a set by label

`--label` (short `-l`, repeatable) selects every image carrying that label. If you also pass names, an image must match a name **and** a label to be selected.

```bash
# Build everything labelled "ci"
cuenv build --label ci --path examples/container-image --package examples
```

## Local vs. registry behaviour

What `cuenv build` actually runs depends on whether the image declares a `registry`.

| Source | No `registry` (local) | `registry` set (push) |
| --- | --- | --- |
| Dockerfile (`context`) | `docker build -t <name>:<tag> <context>` | `docker buildx build --push -t <registry>/<repo>:<tag> <context>` |
| Nix (`installable`) | `nix build` then `docker load` then `docker tag` | `nix build`, `docker load`, `docker tag`, then `docker push` |

A few rules `cuenv build` enforces before it ever shells out:

- **Multi-platform requires a registry.** A Dockerfile image with more than one `platform` and no `registry` is rejected — a local Docker daemon cannot hold a multi-arch manifest. Add a `registry` to push it.
- **A registry needs tags.** An image with a `registry` but no `tags` is rejected; add at least one tag to push.
- **Nix images deliver through Docker.** The flake output is built with `nix build --no-link --print-out-paths`, the resulting archive is `docker load`-ed, then each configured reference is tagged (and pushed when a registry is set).

So a Dockerfile image destined for a registry looks like this:

```cue
images: {
    api: schema.#ContainerImage & {
        context: "."
        tags: ["v1.0.0", "latest"]
        registry:   "ghcr.io/myorg"
        repository: "myorg/api"
        platform: ["linux/amd64", "linux/arm64"]
    }
}
```

```bash
cuenv build api --path . --package cuenv
# -> docker buildx build --push --platform linux/amd64,linux/arm64 \
#      -f ./Dockerfile -t ghcr.io/myorg/myorg/api:v1.0.0 -t ghcr.io/myorg/myorg/api:latest .
```

:::tip[Prerequisites]
`cuenv build` calls `docker` (and `docker buildx` for pushes) on your `PATH`. Registry pushes assume you are already authenticated (`docker login`). Multi-platform builds need a `buildx` builder that supports the requested platforms.
:::

## Chaining images together

`dependsOn` accepts tasks **and other images**, so you can build a base image and then a downstream image that consumes it. Pair this with `buildArgs`, whose values may be plain strings or an image **output reference** (`images.base.ref`):

```cue
images: {
    base: schema.#ContainerImage & {
        context: "images/base"
        tags: ["latest"]
    }

    app: schema.#ContainerImage & {
        context: "images/app"
        tags: ["latest"]
        dependsOn: [images.base]
        buildArgs: {
            BASE_IMAGE: images.base.ref
        }
    }
}
```

The DAG ordering is honoured — `base` builds before `app` — and the dependency is declared in one place instead of an implicit ordering buried in a script.

## Honesty: the partial edges

This feature is shipping but incomplete. Know the edges before you lean on them:

- **Downstream output references are partial.** An image's `.ref` and `.digest` are designed to be resolved at runtime and consumed by tasks and other images. That resolution is **incomplete**: a `buildArg` referencing an unresolved image output reference fails the build with an explicit error rather than silently producing a wrong value. Treat cross-image `.ref` / `.digest` wiring as preview, not stable.
- **Multi-arch Nix images are unsupported.** The Nix path builds a single output archive and loads it with `docker load`. There is no multi-platform manifest assembly for Nix images — use `platform` only on Dockerfile images with a registry.
- **Image-backed service dependencies fail fast.** A `#Service` whose `dependsOn` includes a `#ContainerImage` causes `cuenv up` to abort before doing work, because image execution backends are not wired into service startup yet. Build the image separately and reference the resulting process. See [Run services](/how-to/services/).
- **No Dagger execution yet.** Builds run through the local Docker CLI; the Dagger backend for images is not implemented.

The authoritative, current status is always [Schema status](/reference/schema/status/) and the [schema coverage matrix](https://github.com/cuenv/cuenv/blob/main/docs/design/specs/schema-coverage-matrix.md). If those say something narrower than this page, believe them.

## Where to go next

- [CLI reference: `cuenv build`](/reference/cli/) — every flag and the exact invocation rules.
- [Run services](/how-to/services/) — long-running processes, and the path toward image-backed services.
- [Run tasks](/how-to/run-tasks/) — define the tasks your images depend on, like `codegen`.
- [Codegen](/how-to/codegen/) — generate sources before an image build.
- [Schema status](/reference/schema/status/) — the source of truth for what is implemented, partial, or schema-only.
