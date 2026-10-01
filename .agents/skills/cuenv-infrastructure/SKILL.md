---
name: cuenv-infrastructure
description: Use for cuenv infrastructure as code — the project `infrastructure` block, Terraform provider plugins driven over gRPC, typed provider schemas from the github.com/cuenv/terraform CUE registry modules, Turso state keyed by CUE module path, project and optional named environment, with a separate legacy no-flag namespace, and the `cuenv infrastructure` (short form `cuenv i`) plan, apply, destroy, state and unlock commands. Covers schema/infrastructure.cue.
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
- Infrastructure evaluation requires common state plus the selected named configuration to be concrete. Without `--env`, it requires the legacy state/providers/resources that are present; unselected named configurations are pruned before strict infrastructure DTO deserialization. The infrastructure object is exported leniently so Rust DTO validation still rejects unknown top-level fields. Ordinary Project decoding keeps infrastructure raw, so unused incomplete named environments do not break task discovery, sync or CI.
- Provider `version` and the module major version must match.
- CUE keeps provider and resource objects closed and validates their typed configuration. Before reading state or launching any provider, the Rust engine checks every provider source and install choice, every resource's provider, and all `dependsOn` entries and cycles in the selected configuration. It requires exactly one provider `version` or `path`, including for unused provider declarations.
- When the package name differs from the last import path element (`hashicorp/google-beta` → `google_beta`), the import needs an explicit qualifier: `…/google-beta@v8:google_beta`.
- CUE errors usually, but not always, carry a file position; do not promise one.
- In-repository examples stay untyped so they evaluate without network access in the Nix sandbox.

## Named environments and secrets

- Global `--env NAME` selects the complete `infrastructure.environments.NAME` provider/resource set and, when present, the matching `env.environment.NAME` overlay. The common state backend is outside the selection; do not deep-merge with top-level providers/resources. Unknown or incomplete selections must fail before backend/provider side effects.
- Project secret values use Cuenv's existing runtime resolvers. `allowInfrastructure` names exact command actions (`plan`, `apply`, `destroy`, `state-list`, `state-remove`, `state-recover`, `state-adopt`, `unlock`). A denied project variable must also be withheld from provider host-environment inheritance.
- The plan digest must distinguish legacy, explicit named environments and resolved provider-environment identity without exposing environment values.

## Status guardrails

- `#Infrastructure` is a partial proof of concept. Do not present it as a Terraform replacement.
- State is keyed by CUE module path and project name, plus an optional named environment. `TenantKey::new` preserves the legacy no-flag namespace; `TenantKey::with_environment` creates an isolated named namespace, including explicit `default`. Never fall back between them. `plan`, `apply` and `destroy` still refuse a project name shared with any other infrastructure instance in any directory or CUE package of the module (child directories inherit their parent's `name`), failing closed when any instance cannot be evaluated; `state` and `unlock` skip the uniqueness check. Tenancy is a naming boundary, not a security boundary: anyone with the database token can reach every tenant.
- Legacy owners remain in `cuenv_infrastructure_owners`; named-environment owners use `cuenv_infrastructure_environment_owners`, each binding that state identity to one CUE instance (`<directory>:<package>`). Never bypass ownership. Moving a project means `cuenv i state adopt`, not deleting the row.
- Interactive `apply` and `destroy` hold the lock from planning through the confirmation prompt, as Terraform does; there is no re-plan. Declining exits 1.
- `state recover` compares the generation and serial the unrecorded file replaced. Conditional creates preserve their payload generation. Format-v4 files also save a hash of the normalized Turso backend; ordinary recovery checks the saved, wrapper and actual store identities before any writes. Retries acknowledge only the same generation, resulting serial and content. Format-v2 lacks generations; v2/v3 lack backend binding. These files remain readable but require inspected force where comparisons are unsafe, with a CLI warning. `--force` is for an operator who has checked the saved object, stored object and backend, never a default.
- Every state write is fenced by the caller's lock (`StateStore::put`/`delete` take the `StateLock`); never add an unfenced write path. Failed creates are recorded as tainted; failed updates retain the union of stored and desired dependency edges. Changes that cannot be recorded are saved under the user state directory (`cuenv/infrastructure/unrecorded/`, never the project tree) and re-recorded with `cuenv i state recover`; `plan`, `apply` and `destroy` refuse to run while such files exist. Errors never carry state values.
- Provider processes preserve the caller environment, overlay resolved and policy-authorized project variables, then remove every denied project variable and the state token. `allowInfrastructure` filters before secret resolution; state-only actions resolve only the configured backend token. Register all resolved secret parts for redaction before state/provider output. Provider-origin text must never reach raw tracing sinks; trace metadata only.
- Interrupts follow Terraform: the first SIGINT, SIGTERM, SIGHUP or SIGQUIT asks running providers to stop and records what they return; the second kills provider process groups, waits briefly for a record in flight, releases the lock within a bound and prints the lock identifier (registered before acquisition, so a lock taken during the interrupt is still reported). Never add an exit path that skips recording or leaves providers running.
- Each legacy resource is one row in `cuenv_infrastructure_resources`; named-environment resources use `cuenv_infrastructure_environment_resources`, keyed by module, project, environment and address, with no cross-family fallback. Schema v5 backfills a generation UUID on both table families; updates retain it, deletion and recreation allocate a fresh one. State is `cty` JSON so `UpgradeResourceState` can migrate it. Do not store MessagePack or re-encode planned states before `ApplyResourceChange`.
- Replacement deletes and their removed dependents run in reverse stored dependency order before forward creation. In-place detachments and their full desired prerequisite closure run before early deletes; an unavailable create/replacement prerequisite fails before provider mutations. Other orphan deletes remain after desired convergence. Record each provider result before proceeding; a failed dependent delete must leave its parent intact, and an interrupted replacement can resume from absent state. An interrupted apply without an RPC response must identify its unknown resource outcome.
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
- "Share state between environments." Each explicit environment has separate rows, locks and ownership; only runs with the same module path, project name and environment share that state. No-flag legacy state is distinct.
- "Write `random.#Resource_random_pet` inside `providers: random:`." Explain the shadowing pitfall and use an alias.
