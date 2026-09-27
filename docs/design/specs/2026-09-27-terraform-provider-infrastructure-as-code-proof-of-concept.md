# Infrastructure as Code through Terraform Provider Plugins (Proof of Concept)

Status: proof of concept, implemented in `crates/infrastructure`
(`cuenv-infrastructure`) and `cuenv infrastructure` (short form `cuenv i`).

## Goal

Manage infrastructure from CUE the cuenv way — typed configuration, one tool —
while reusing the Terraform and OpenTofu provider ecosystem unchanged. No
Terraform or OpenTofu command line tool, no HCL, no state files.

## Naming

No abbreviations: `#Infrastructure`, `infrastructure`, `configuration`,
`authenticationTokenEnvironmentVariable`, `cuenv infrastructure`, crate
`cuenv-infrastructure`, tables `cuenv_infrastructure_*`. The only short form
is the command alias `cuenv i`. Established acronyms and external names
(JSON, SQL, gRPC, Terraform's `cty`, Hrana wire keys) are unchanged.

## Decisions

### Talk to providers directly over gRPC

Providers are HashiCorp `go-plugin` servers. cuenv launches the binary with
`TF_PLUGIN_MAGIC_COOKIE` and `PLUGIN_PROTOCOL_VERSIONS=5,6`, reads the
handshake line (`1|<protocol>|unix|<socket>|grpc|`), and dials the unix socket
with tonic. Mutual TLS is not requested, so the socket is plaintext and
private to the user, matching Terraform with plugin TLS disabled.

Protocols 5 and 6 are both supported. They are wire-identical for every
managed-resource procedure cuenv uses; only procedure names and the schema
`Attribute` message (tag 10) differ. Most HashiCorp utility providers still
serve protocol 5; Plugin Framework providers such as `cloudflare/cloudflare`
version 5 and `hashicorp/tfe` serve protocol 6.

The protobuf messages are hand-written prost structures covering only the
fields cuenv reads (`crates/infrastructure/src/protocol.rs`). That avoids a
`protoc` build dependency and a vendored generated file; unknown fields are
skipped by protobuf decoding.

### Implement enough of Terraform's `cty` type system

Provider values are type-directed MessagePack: identical bytes decode
differently depending on the schema type. `crates/infrastructure/src/type_system.rs`
implements type parsing, CUE JSON → typed value conversion (with Terraform's
primitive conversions), MessagePack encoding and decoding including unknown
values (extension 0) and `dynamic` wrappers, set-order-insensitive equality,
and `cty` JSON for state.

Planned states are passed back to `ApplyResourceChange` as the provider's
original bytes, never re-encoded, so unknown-value refinements survive.

### Terraform core lifecycle, per resource

1. Refresh: `UpgradeResourceState` (stored JSON and schema version) then
   `ReadResource`.
2. Plan: `ValidateResourceConfig`, then `PlanResourceChange` with a proposed
   new state (simplified `objchange.ProposedNew`). Non-empty
   `requires_replace` becomes destroy-then-create.
3. Apply: `ApplyResourceChange`; the resulting state is written immediately,
   including partial state returned alongside error diagnostics. A null or
   unknown result alongside errors keeps whatever was recorded.

Declared resources are ordered by `dependsOn`; orphans (recorded but no
longer declared) are deleted first in reverse dependency order.

### State: one Turso row per managed resource, keyed by tenant

```sql
cuenv_infrastructure_resources(
  module_path, project, resource_type, resource_name,   -- primary key
  provider, provider_source, schema_version,
  state_json, private, dependencies_json, serial, created_at, updated_at)

cuenv_infrastructure_locks(module_path, project, lock_identifier, holder, acquired_at)
```

- The tenant is the CUE module path from `cue.mod/module.cue` (major-version
  suffix stripped); the project name discriminates within it. `TenantKey`
  cannot be constructed without both, and every `StateStore` method takes one.
- Tenancy is a naming boundary, not a security boundary: the module path is
  declared by the project, so anyone with the database token can reach every
  tenant. Separate databases per tenant give real isolation.
- Rows store `cty` JSON, exactly what `UpgradeResourceState` consumes, so state
  written by one provider version is upgraded by the next.
- The store speaks Hrana over HTTP (`POST /v2/pipeline`) with reqwest, which
  works for Turso Cloud and self-hosted `sqld` without a native libSQL
  dependency.
- Locks are `INSERT … ON CONFLICT DO NOTHING`; zero affected rows means
  another holder, which is reported with its identity.

### Provider installation

`hashicorp/random` plus an exact version resolves via registry service
discovery (`/.well-known/terraform.json`), downloads the platform archive,
verifies the registry-reported SHA-256, and extracts into Terraform's plugin
cache layout (reusing `TF_PLUGIN_CACHE_DIR` when set). A local `path`
bypasses the registry.

### Typed configuration from the CUE registry

https://github.com/cuenv/terraform generates CUE from each provider release's
schema and publishes it to the CUE registry as
`github.com/cuenv/terraform/terraform/<namespace>/<type>@v<major>` (package
`<type>`, closed definitions `#ProviderConfig` and `#Resource_<type>`). The
cuenv schema keeps `configuration` open (`{...}`) so projects unify it with
those definitions; cuenv's own schema does not import provider modules, which
keeps it independent of provider releases.

Verified end to end: a project importing
`github.com/cuenv/terraform/terraform/hashicorp/random@v3` at `v3.9.1`
evaluated, rejected mistyped and unknown arguments at evaluation time, and
applied and destroyed `random_pet` through `cuenv i`.

Pitfall found during that verification: an import named `random` is shadowed
inside `providers: random: {...}`. cuenv's Go bridge then exports the
undefined reference as `null` instead of failing. Documentation prescribes an
alias (`randomProvider`); the bridge behaviour is tracked below.

## Review practice

Every milestone gets an adversarial review from five personas in parallel —
security engineer, platform operator, Terraform protocol specialist, CUE and
schema designer, cuenv maintainer — with each finding verified before it is
acted on. The persona briefs live in `.agents/skills/cuenv-infrastructure/SKILL.md`.

## Validation

- Unit tests: `cty` codec and set equality, schema conversion, normalization
  and proposed state, handshake parsing, procedure naming, Hrana wire format,
  tenant parsing, memory store isolation and locking, registry cache layout and
  archive extraction, diagnostics, orphan ordering and plan rendering.
- Ignored integration tests (`crates/infrastructure/tests/provider_end_to_end.rs`)
  run the full lifecycle — create, idempotent re-plan, forced replacement,
  orphan delete, destroy, tenant isolation — against real `hashicorp/random`
  and `hashicorp/local` binaries with both the in-memory store and `sqld`; load
  a protocol 6 schema from `hashicorp/tfe`; and install from the live registry.

## Next steps

1. Cross-resource references. The real value of CUE here: let a resource's
   configuration reference another's attributes, propagate unknowns through
   planning, and resolve them during apply in dependency order. Derive
   `dependsOn` from references.
2. Make cuenv's Go bridge fail evaluation when a field is an error instead of
   exporting it as `null` (`crates/cuengine/values.go`, `buildValueClean`).
   This changes evaluation for every project, so it needs its own change.
3. Data sources (`ReadDataSource`) and imports (`ImportResourceState`).
4. Parallel apply across independent resources.
5. Provider version constraints, lock file entries in `cuenv.lock`, and GPG
   verification of `SHA256SUMS`.
6. Secret-typed provider and resource arguments resolved through cuenv's
   secret resolvers instead of plaintext CUE or ambient environment.
7. Full `objchange.ProposedNew` semantics for nested block collections and
   write-only attributes.
8. Lock leases with expiry instead of manual `cuenv i unlock`.
9. In github.com/cuenv/terraform, `#ProviderConfig` abbreviates
   "configuration"; renaming it to `#ProviderConfiguration` would bring the
   generated modules in line with the no-abbreviation rule.
