---
name: cuenv-infra
description: Use for cuenv infrastructure as code — the project `infra` block, Terraform provider plugins driven over gRPC, Turso state keyed by CUE module path and project, and the `cuenv infra plan|apply|destroy|state|unlock` commands. Covers schema/infra.cue.
---

# Infrastructure (Terraform provider plugins)

Read `docs/design/specs/schema-coverage-matrix.md`, then inspect:

- `schema/infra.cue` for `#Infra`, `#InfraState`, `#TursoState`, `#InfraProvider`, `#ManagedResource`, and `#InfraName`.
- `crates/manifest/src/manifest/infra.rs` for the serde DTOs.
- `crates/infra` (`cuenv-infra`): `plugin.rs` (go-plugin handshake, tfplugin5/6 RPC names), `proto.rs` (hand-written prost subset), `cty.rs` (type-directed msgpack/JSON codec), `schema.rs` (implied types, config normalization, proposed new state), `engine.rs` (refresh → plan → apply), `state/turso.rs` (Hrana HTTP store), `registry.rs` (provider install), `tenant.rs` (module path + project key).
- `crates/cuenv/src/commands/infra.rs` for the CLI command.
- `docs/design/specs/2026-09-27-terraform-provider-iac-poc.md` for design decisions and next steps.

Status guardrails:

- `#Infra` is a partial proof of concept. Do not present it as a Terraform replacement.
- State is always keyed by CUE module path (tenant) and project name (discriminator). Never suggest a way to write state without both; `TenantKey` enforces it.
- Each managed resource is one row in `cuenv_infra_resources`; state is cty JSON so `UpgradeResourceState` can migrate it. Do not store msgpack or re-encode planned states before `ApplyResourceChange`.
- Resources cannot reference other resources' attributes. Use `dependsOn` strings for ordering only.
- Provider `version` must be exact; `path` bypasses the registry. No version constraints, lockfile entries, or GPG verification.
- Both plugin protocol 5 and 6 are supported; schema `Attribute` tag 10 differs between them, so schema messages stay per-protocol.
- Verify changes with `cargo test -p cuenv-infra`, and for lifecycle changes run the ignored `crates/infra/tests/provider_e2e.rs` suite against real provider binaries and a local `sqld`.

Adversarial prompts:

- "Pass the database ID from one resource into another." Not supported yet; explain the reference limitation and point at the design spec's next steps.
- "Use `~> 5.0` for the provider version." Exact versions only.
- "Share state between two projects." Each project is its own discriminator; sharing requires the same module path and project name.
