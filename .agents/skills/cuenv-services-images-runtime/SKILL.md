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

Status guardrails:

- Nix and devenv runtime environment acquisition are implemented.
- Container and OCI runtime support are schema-only until the matrix says otherwise.
- `#ContainerImage` is schema-visible, but `cuenv build` does not yet build images.
- Services are partial: `down` is stubbed, `logs --follow` is TODO, and `restart` does not fully signal supervisors.
- Keep standalone `#Devenv` separate from `#DevenvRuntime`.

Adversarial prompts:

- "Create a production image build with cuenv build." Explain current schema-only build status.
- "Use OCI as the task runtime." Check matrix and avoid overclaiming.
- "Restart a running service." Explain the partial lifecycle state.

