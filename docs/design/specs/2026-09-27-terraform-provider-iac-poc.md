# Infrastructure as Code via Terraform Provider Plugins (Proof of Concept)

Status: proof of concept, implemented in `crates/infra` (`cuenv-infra`) and
`cuenv infra`.

## Goal

Manage infrastructure from CUE the cuenv way — typed configuration, one tool —
while reusing the Terraform/OpenTofu provider ecosystem unchanged. No
Terraform or OpenTofu CLI, no HCL, no state files.

## Decisions

### Talk to providers directly over gRPC

Providers are HashiCorp `go-plugin` servers. cuenv launches the binary with
`TF_PLUGIN_MAGIC_COOKIE` and `PLUGIN_PROTOCOL_VERSIONS=5,6`, reads the
handshake line (`1|<proto>|unix|<socket>|grpc|`), and dials the unix socket
with tonic. AutoMTLS is not requested, so the socket is plaintext and private
to the user, matching Terraform with plugin TLS disabled.

Protocols 5 and 6 are both supported. They are wire-identical for every
managed-resource RPC cuenv uses; only RPC names and the schema `Attribute`
message (tag 10) differ. Most HashiCorp utility providers still serve v5;
Plugin Framework providers such as `cloudflare/cloudflare` v5 and
`hashicorp/tfe` serve v6.

The protobuf messages are hand-written prost structs covering only the fields
cuenv reads (`crates/infra/src/proto.rs`). That avoids a `protoc` build
dependency and a vendored generated file; unknown fields are skipped by
protobuf decoding.

### Implement enough of `cty`

Provider values are type-directed msgpack: identical bytes decode differently
depending on the schema type. `crates/infra/src/cty.rs` implements type
parsing, CUE JSON → typed value conversion (with Terraform's primitive
conversions), msgpack encode/decode including unknown values (extension 0)
and `dynamic` wrappers, and cty JSON for state.

Planned states are passed back to `ApplyResourceChange` as the provider's
original bytes, never re-encoded, so unknown-value refinements survive.

### Terraform-core lifecycle, per resource

1. Refresh: `UpgradeResourceState` (stored JSON + schema version) then
   `ReadResource`.
2. Plan: `ValidateResourceConfig`, then `PlanResourceChange` with a proposed
   new state (simplified `objchange.ProposedNew`). Non-empty
   `requires_replace` becomes destroy-then-create.
3. Apply: `ApplyResourceChange`; the resulting state is written immediately,
   including partial state returned alongside error diagnostics.

Declared resources are ordered by `dependsOn`; orphans (recorded but no
longer declared) are deleted first in reverse dependency order.

### State: one Turso row per managed resource, keyed by tenant

```sql
cuenv_infra_resources(
  module_path, project, resource_type, resource_name,   -- primary key
  provider, provider_source, schema_version,
  state_json, private, dependencies_json, serial, created_at, updated_at)

cuenv_infra_locks(module_path, project, lock_id, holder, acquired_at)
```

- The tenant is the CUE module path from `cue.mod/module.cue` (major-version
  suffix stripped); the project name discriminates within it. `TenantKey`
  cannot be constructed without both, and every `StateStore` method takes one.
- Rows store cty JSON, exactly what `UpgradeResourceState` consumes, so state
  written by one provider version is upgraded by the next.
- The store speaks Hrana over HTTP (`POST /v2/pipeline`) with reqwest, which
  works for Turso Cloud and self-hosted `sqld` without a native libSQL
  dependency.
- Locks are `INSERT … ON CONFLICT DO NOTHING`; zero affected rows means
  another holder, which is reported with its identity.

### Provider installation

`hashicorp/random` + exact version resolves via registry service discovery
(`/.well-known/terraform.json`), downloads the platform zip, verifies the
registry-reported SHA-256, and extracts into Terraform's plugin cache layout
(reusing `TF_PLUGIN_CACHE_DIR` when set). A local `path` bypasses the
registry.

## Validation

- Unit tests: cty codec, schema conversion/normalization/proposed state,
  handshake parsing, RPC naming, Hrana wire format, tenant parsing, memory
  store isolation and locking, registry cache layout and zip extraction,
  diagnostics and plan rendering.
- Ignored integration tests (`crates/infra/tests/provider_e2e.rs`) run the
  full lifecycle — create, idempotent re-plan, forced replacement, orphan
  delete, destroy, tenant isolation — against real `hashicorp/random` and
  `hashicorp/local` binaries with both the in-memory store and `sqld`; load a
  protocol 6 schema from `hashicorp/tfe`; and install from the live registry.

## Next steps

1. Cross-resource references. The real value of CUE here: let a resource's
   config reference another's attributes, propagate unknowns through
   planning, and resolve them during apply in dependency order. Derive
   `dependsOn` from references.
2. Data sources (`ReadDataSource`) and imports (`ImportResourceState`).
3. Parallel apply across independent resources.
4. Provider version constraints, lock file entries in `cuenv.lock`, and GPG
   verification of `SHA256SUMS`.
5. Secret-typed provider and resource arguments resolved through cuenv's
   secret resolvers instead of plaintext CUE or ambient environment.
6. Full `objchange.ProposedNew` semantics for nested block collections and
   write-only attributes.
7. Lock leases with expiry instead of manual `cuenv infra unlock`.
