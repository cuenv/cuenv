# Container image building for `cuenv build` (Docker/Buildx + Nix)

**Date:** 2026-05-28
**Status:** Design — pending implementation plan
**Scope:** Build execution only. Images are NOT yet task-DAG nodes; `.ref`/`.digest` output references remain unpopulated.

## Problem

`cuenv build` should build the container images declared in CUE. We support two ways to define an image:

1. **Dockerfile images** — a build context + Dockerfile (already implemented on this branch).
2. **Nix-native images** — the image is defined by a Nix flake output (`installable`), not a Dockerfile.

Both are delivered by shelling out to existing tools. cuenv depends on `docker` + `buildx` (and `nix` for the Nix path) being installed. We are **not** building any in-process OCI/registry machinery.

## Constraints and decisions

- **Depend on `docker` and `buildx`.** Shelling out to them is the chosen, accepted approach. This supersedes an earlier exploration of an in-process Rust implementation (bollard + `oci-client` + a hand-rolled push pipeline), which was rejected as too much machinery for the value.
- **Depend on `nix`** for the Nix path. cuenv already invokes `nix` elsewhere (`crates/tools/nix`).
- **No `skopeo` dependency.** The Nix path delivers images through `docker` (`docker load` + `docker push`), keeping the tool surface to docker/buildx/nix.
- **No new crate, no backend trait, no factory injection.** The only consumer is `crates/cuenv/src/commands/build.rs` in the binary crate. Image building is a sequence of subprocess calls; it lives in `build.rs` (optionally split into a small `commands/build/` module) and branches on the image kind.
- **Build-only scope.** No DAG integration, no output-reference resolution, no `inputs`-based caching in this iteration.

## Schema changes

Add an optional `installable?` field to `#ContainerImage`, **mutually exclusive** with the Dockerfile fields (`context`, `dockerfile`, `buildArgs`, `target`, `platform`) via a CUE constraint. An image is therefore *either* Dockerfile-defined *or* Nix-defined, never both.

- **Why an optional field, not a tagged union:** a serde internally-tagged enum (`#[serde(tag="type")]`) cannot express the existing `type` default (`"image"`), so configs that omit `type` would fail to deserialize. An optional field keeps the existing `ContainerImage` struct and serde defaults intact and confines the Rust change to a single branch in `build.rs` on `image.installable.is_some()`.
- Nix images reuse `registry` / `repository` / `tags` / `labels` for the destination reference and discovery.
- Update `schema/images.cue` docs and `docs/design/specs/schema-coverage-matrix.md`. Run `cuenv task ci.schema-docs-check`.

## Build flow

`build.rs` selects images (existing name/label filtering), then for each builds by kind.

### Dockerfile images (`context` set) — unchanged

Keep the existing `DockerBuildInvocation` logic already on this branch:

- Local (no `registry`): `docker build -f <context>/<dockerfile> [--platform p] [--target t] [--build-arg k=v ...] -t <repo>:<tag> ... <context>`.
- Registry set: `docker buildx build --push --platform <list> ... -t <registry>/<repo>:<tag> ... <context>`.

This already includes the two correctness fixes from the prior review: a guard rejecting a registry image with no tags, and deterministic `--build-arg` ordering. **Multi-arch is supported here** via `buildx --platform a,b --push` (no regression).

### Nix-native images (`installable` set) — new

1. **Build:** run `nix build <installable>` producing a Docker image archive. Pin the expected Nix builder to `dockerTools.buildLayeredImage` / `streamLayeredImage` (produces a `docker load`-compatible archive). The image derivation lives in the user's flake; cuenv only references it via `installable`.
2. **Local (no `registry`):** `docker load` the archive into the daemon, then `docker tag` it to the configured `repository:tag`(s).
3. **Registry set:** `docker load` → `docker tag <registry>/<repository>:<tag>` → `docker push` for each tag.

The destination reference is assembled from the same `registry`/`repository`/`tags` fields used by the Dockerfile path, so `image_refs` logic is shared between both kinds.

**Dual source-of-truth note:** a Nix image's build is defined in the flake while its *naming/push* is defined in CUE. The mutual-exclusion constraint keeps a single image from being half-Dockerfile/half-Nix, but users do maintain the image build in Nix and its publishing metadata in CUE. This is accepted.

## Architecture

- All logic in `crates/cuenv/src/commands/build.rs` (or a `commands/build/` submodule if it grows): `DockerBuildInvocation` (existing) and a new `NixBuildInvocation` that wraps the `nix build` + `docker load/tag/push` sequence. Dispatch by `image.installable.is_some()`.
- Subprocess execution follows the existing pattern (`std::process::Command`, status checked, output via `cuenv_events` — never `println!`).
- `cuenv-core` keeps the image data types (`ContainerImage` gains `installable`). No other crates change.

## Error handling

- Missing `docker` / `buildx` / `nix` binary → clear, actionable error naming the missing tool.
- `nix build` failure → surface the installable and nix's exit status.
- Registry image with no tags → existing guard (already applied).
- Each subprocess non-zero exit → error including the command and status.

## Testing

- **Schema:** round-trip deserialize a `#ContainerImage` with `installable` set and one with `context` set; assert the CUE mutual-exclusion constraint rejects setting both.
- **Invocation construction (unit, no daemon/network):** assert the exact argv for the Nix path — `nix build` args, then the `docker load`/`tag`/`push` sequence for local vs registry, with multi-tag fan-out — mirroring the existing `DockerBuildInvocation` argv tests.
- **Dispatch (unit):** `build.rs` selects the Nix vs Dockerfile path by `installable`.
- **Integration (gated/ignored):** an end-to-end build against a real daemon, behind an ignored/CI-only test since it needs `docker`/`nix`.

## Deferred (explicitly out of scope)

- Images as task-DAG nodes; `dependsOn` ordering; `inputs`-based content caching.
- Populating `.ref` / `.digest` output references for downstream consumption.
- Multi-architecture **Nix** images (would need multiple per-arch installables + a manifest list). Dockerfile multi-arch via buildx is supported.
- In-process / daemonless image building (bollard, `oci-client`, skopeo) — rejected in favor of the docker/buildx/nix CLIs.
