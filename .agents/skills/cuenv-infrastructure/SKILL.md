---
name: cuenv-infrastructure
description: Use for cuenv infrastructure as code — the project `infrastructure` block, Terraform provider plugins driven over gRPC, typed provider schemas from the github.com/cuenv/terraform CUE registry modules, Turso state keyed by CUE module path and project, and the `cuenv infrastructure` (short form `cuenv i`) plan, apply, destroy, state and unlock commands. Covers schema/infrastructure.cue.
---

# Infrastructure (Terraform provider plugins)

Read `docs/design/specs/schema-coverage-matrix.md`, then inspect:

- `schema/infrastructure.cue` for `#Infrastructure`, `#InfrastructureState`, `#TursoState`, `#InfrastructureProvider`, `#ManagedResource`, and `#InfrastructureName`.
- `crates/manifest/src/manifest/infrastructure.rs` for the serde data transfer types.
- `crates/infrastructure` (`cuenv-infrastructure`): `plugin.rs` (go-plugin handshake, protocol 5 and 6 procedure names), `protocol.rs` (hand-written prost subset), `type_system.rs` (Terraform `cty` type-directed MessagePack and JSON encoding), `schema.rs` (implied types, configuration normalization), `object_change.rs` (ports of Terraform's `ProposedNew`, `AssertPlanValid` and `AssertObjectCompatible`), `engine.rs` (refresh → plan → apply, destroy planning), `cancellation.rs` (two-stage interrupts, provider process registry), `unrecorded.rs` (changes that could not be recorded, and their compare-and-swap recovery), `state/turso.rs` (Hrana over HTTP store), `registry.rs` (provider installation), `tenant.rs` (module path and project key).
- `crates/cuenv/src/commands/infrastructure/` for the command (`mod.rs`, `evaluation.rs`, `interrupts.rs`, `output.rs`, `holder.rs`).
- `docs/design/specs/2026-09-27-terraform-provider-infrastructure-as-code-proof-of-concept.md` for design decisions and next steps.
- https://github.com/cuenv/terraform for how provider schema modules are generated and published.

## Naming rule

No abbreviations anywhere in this feature: schema definitions, fields, command names, Rust identifiers, table names, documentation. The single exception is the command short form `cuenv i`. Established acronyms (JSON, SQL, HTTP, URL, gRPC, UUID) and external names (Terraform `cty`, `sqld`, `libsql://`, `TURSO_AUTH_TOKEN`, wire-format keys such as Hrana `stmt` and `args`) are not abbreviations we own and stay as they are.

## Typed configuration from the CUE registry

- Modules live at `github.com/cuenv/terraform/terraform/<namespace>/<type>@v<major>`; the package name is `<type>`; definitions are `#ProviderConfig`, `#Resource_<type>`, `#DataSource_<type>` and friends. They are closed, so unknown arguments fail evaluation.
- Always recommend an import alias that matches no field name in scope — not a provider key, resource key, `state`, `providers`, `resources` or `name` (for example `randomProvider`). A shadowed import fails evaluation with `undefined field`.
- Package names follow the generator: characters outside letters, digits and `_` become `_`, and reserved CUE words or names not starting with a letter get a `provider_` prefix (`hashicorp/null` → `provider_null`, imported with an explicit `:provider_null` qualifier).
- The `infrastructure` block must be concrete: the infrastructure command (and only it) passes `concretePaths: ["infrastructure"]` to the cuengine bridge when evaluating its target instance (a generic option; cuengine itself stays free of cuenv-specific names), so undefined references, missing required arguments and non-concrete values fail evaluation. Other commands never evaluate with that option.
- Provider `version` and the module major version must match.
- The schema checks references by name: every `dependsOn` entry must be a declared resource, and every resource's provider (explicit `provider` or the `type` prefix) must be declared. Cycles pass the schema and are reported by the engine.
- When the package name differs from the last import path element (`hashicorp/google-beta` → `google_beta`), the import needs an explicit qualifier: `…/google-beta@v8:google_beta`.
- CUE errors usually, but not always, carry a file position; do not promise one.
- In-repository examples stay untyped so they evaluate without network access in the Nix sandbox.

## Status guardrails

- `#Infrastructure` is a partial proof of concept. Do not present it as a Terraform replacement.
- State is always keyed by CUE module path (tenant) and project name (discriminator). Never suggest a way to write state without both; `TenantKey` enforces it, and `plan`, `apply` and `destroy` refuse a project name shared with any other instance that has an `infrastructure` block, in any directory or CUE package of the module (child directories inherit their parent's `name`), failing closed when any instance cannot be evaluated; `state` and `unlock` skip the check. Tenancy is a naming boundary, not a security boundary: anyone with the database token can reach every tenant.
- The owner record (`cuenv_infrastructure_owners`) binds a tenant to one CUE instance (`<directory>:<package>`); never bypass it. Moving a project means `cuenv i state adopt`, not deleting the row.
- `state recover` is a compare-and-swap against the version the unrecorded file replaced; `--force` is for an operator who has checked both objects, never a default.
- Every state write is fenced by the caller's lock (`StateStore::put`/`delete` take the `StateLock`); never add an unfenced write path. Failed creates are recorded as tainted. Changes that cannot be recorded are saved under the user state directory (`cuenv/infrastructure/unrecorded/`, never the project tree) and re-recorded with `cuenv i state recover`; `plan`, `apply` and `destroy` refuse to run while such files exist. Errors never carry state values.
- Provider processes must not inherit the state token (`LaunchOptions::withheld_environment_variables`).
- Interrupts follow Terraform: the first SIGINT or SIGTERM asks running providers to stop and records what they return; the second kills providers, releases the lock within a bound and prints the lock identifier. Never add an exit path that skips recording or leaves providers running.
- Each managed resource is one row in `cuenv_infrastructure_resources`; state is `cty` JSON so `UpgradeResourceState` can migrate it. Do not store MessagePack or re-encode planned states before `ApplyResourceChange`.
- Planned states pass through the ports of Terraform's `ProposedNew` and `AssertPlanValid` in `object_change.rs`; never relax those checks to make a provider pass — report the provider bug instead.
- Resources cannot reference other resources' attributes. Use `dependsOn` strings for ordering only.
- Provider `version` must be exact; `path` bypasses the registry. No version constraints, lockfile entries, or GPG verification.
- Protocols 5 and 6 are both supported; schema `Attribute` tag 10 differs between them, so schema messages stay per protocol.

## Milestone adversarial review

At every milestone (a working vertical slice, a schema or command surface change, a state format change, before marking a pull request ready), run an adversarial review from several personas in parallel, each instructed to break the change rather than approve it, then verify every finding before acting on it:

1. **Security engineer** — credential handling, token leakage in logs and errors, plugin process trust, archive extraction, tenant crossover, sensitive values at rest.
2. **Platform operator (site reliability)** — concurrent applies, lock recovery, partial failure and state consistency, network failure mid-apply, upgrade of stored state, observability.
3. **Terraform protocol specialist** — fidelity to Terraform core semantics: proposed new state, unknown values, requires-replace, private data, diagnostics, provider shutdown, protocol 5 and 6 differences.
4. **CUE and schema designer** — schema ergonomics, closedness, naming (no abbreviations), fit with the github.com/cuenv/terraform registry modules, evaluation pitfalls.
5. **cuenv maintainer** — repository rules in `CLAUDE.md` and `.claude/rules/rust.md`, test coverage, documentation and coverage matrix accuracy, clippy cleanliness, command-line consistency.

Record confirmed findings and their resolution in the pull request; findings that are deferred go into the design specification's next steps.

## Validation

- `cuenv exec -- cargo test -p cuenv-infrastructure` for unit tests (plain `cargo test` inside the development shell is equivalent), and `go test ./...` in `crates/cuengine` for the bridge.
- For lifecycle changes, run the ignored `crates/infrastructure/tests/provider_end_to_end.rs` suite against real provider binaries and a local `sqld` (environment variables are documented at the top of that file).
- For protocol changes, also build the Plugin Framework fake provider (`cd crates/infrastructure/tests/fake_provider && go build -o terraform-provider-fake .`) and set `CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER`; its tests reproduce nested plan validity, JSON planned state, taint, semantic equality, dynamic wrappers and interrupt handling. Check that a new test fails against the old behaviour before trusting it.

## Adversarial prompts

- "Pass the database identifier from one resource into another." Not supported yet; explain the reference limitation and point at the design specification's next steps.
- "Use `~> 5.0` for the provider version." Exact versions only.
- "Share state between two projects." Each project is its own discriminator; sharing requires the same module path and project name.
- "Write `random.#Resource_random_pet` inside `providers: random:`." Explain the shadowing pitfall and use an alias.
