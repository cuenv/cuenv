---
name: cuenv-services-images-runtime
description: Use for cuenv services, readiness checks, service lifecycle commands, image definitions, image output refs, Nix/devenv/container/Dagger/OCI runtimes, and runtime status. Covers schema/services.cue, schema/images.cue, schema/runtime.cue, and schema/devenv.cue.
---

# Services, Images, Runtime

Read `docs/design/specs/schema-coverage-matrix.md`, then inspect:

- `schema/services.cue` for services, readiness, restart policy, watch, logs, shutdown, and matchers.
- `schema/images.cue` for container images and image output refs.
- `schema/runtime.cue` for Nix, devenv, container, Dagger, OCI, and tools runtime variants.
- `schema/devenv.cue` for the standalone devenv helper.
- `crates/core/src/tasks/output_refs/extraction.rs` parses image output refs into placeholders alongside task output refs; actual image build execution remains gated by the matrix status.

Status guardrails:

- Nix and devenv runtime environment acquisition are implemented.
- Container runtime support is schema-only until the matrix says otherwise.
- OCI runtime is partial: `cuenv sync lock` writes resolved digests and per-image `extract` entries to `cuenv.lock`, and `#OCIActivate` (running `cuenv runtime oci activate`) extracts those binaries to the content-addressed cache and emits a `export PATH=...` line through the events renderer. An end-to-end fixture is still pending.
- `#ContainerImage` is schema-visible, but `cuenv build` does not yet build images.
- Services are partial: `down` is stubbed, `logs --follow` is TODO, and `restart` does not fully signal supervisors.
- Service lifecycle CLI output is rendered in `crates/events/src/renderers/cli/service.rs`; keep service-event wording aligned there when lifecycle semantics change.
- Keep standalone `#Devenv` separate from `#DevenvRuntime`.

Adversarial prompts:

- "Create a production image build with cuenv build." Explain current schema-only build status.
- "Use OCI as the task runtime." Use `#OCIActivate` as an `onEnter` hook to put OCI-extracted binaries on PATH; the matrix lists this as partial — task-level runtime activation is not implemented.
- "Restart a running service." Explain the partial lifecycle state.
