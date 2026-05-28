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
- Nix runtime integration tests in `crates/cuenv/tests/nix_flake_path.rs` and `crates/cuenv/tests/nix_flake_hook.rs` should keep fixture setup, runtime state directories, `Command::new(env!("CARGO_BIN_EXE_cuenv"))` command construction, command execution, and shellHook retry diagnostics behind fallible helpers. Skip missing-Nix, nextest, and sandboxed-FFI cases quietly instead of carrying deprecated test harness APIs, file-level unwrap/expect, or stderr-print allowances.
- Container runtime support is schema-only until the matrix says otherwise.
- OCI runtime is partial: `cuenv sync lock` writes resolved digests and per-image `extract` entries to `cuenv.lock`, and `#OCIActivate` (running `cuenv runtime oci activate`) extracts those binaries to the content-addressed cache and emits a `export PATH=...` line through the events renderer. An end-to-end fixture is still pending.
- `#ContainerImage` is partial: `cuenv build` lists image definitions and builds selected images. An image sets either `context` (Dockerfile, built with docker/buildx — registry builds pushed with buildx) or `installable` (Nix flake output, built with `nix build` then delivered via `docker load`/`tag`/`push`); the two are mutually exclusive. Multi-arch Nix images, Dagger execution, and downstream output-reference resolution remain incomplete.
- Services are partial: `up` honors service-to-service dependencies, includes service deps for selected starts, and rejects task/image dependencies until task execution and image build backends are wired into service startup; `down` can request whole-session shutdown or named-service stop requests from the persisted `cuenv up` session; `logs --follow` tails persisted session logs until the `cuenv up` controller exits; `restart` queues persisted restart requests that running supervisors consume.
- Service orchestration builds a read-only service graph in `crates/services/src/controller.rs`; it should implement `TaskNodeData` for declared dependency names only, not `MutableTaskNodeData`, unless the controller starts injecting dependencies itself.
- Per-service lifecycle decisions live in `crates/services/src/supervisor.rs`; process spawning, command display, env resolution, shutdown, Linux `PR_SET_PDEATHSIG`, and macOS `cuenv __supervise` wrapping live in `crates/services/src/process.rs`. Persisted manual stop/restart request polling lives in `crates/services/src/control.rs`.
- Log readiness probes in `crates/services/src/probes/log.rs` borrow the manifest stream selector; do not clone `l.source` just to pass `"stdout"`, `"stderr"`, or `"either"` through probe construction. `StateUpdate` in `crates/services/src/supervisor.rs` is a copyable value bundle for lifecycle persistence/logging.
- Service lifecycle CLI output is rendered in `crates/events/src/renderers/cli/service.rs`; keep service-event wording aligned there when lifecycle semantics change.
- Keep standalone `#Devenv` separate from `#DevenvRuntime`.

Adversarial prompts:

- "Create a production image build with cuenv build." Explain current partial Docker build support, including registry push behavior, and note that Dagger execution and downstream output-reference resolution remain incomplete.
- "Use OCI as the task runtime." Use `#OCIActivate` as an `onEnter` hook to put OCI-extracted binaries on PATH; the matrix lists this as partial — task-level runtime activation is not implemented.
- "Restart a running service." Use `cuenv restart <service>` with an active `cuenv up` session; the supervisor consumes the persisted restart request.
