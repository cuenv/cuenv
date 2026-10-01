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

Planned states returned as MessagePack are passed back to `ApplyResourceChange`
as the provider's original bytes, so unknown-value refinements survive; prior
states from `ReadResource` are likewise passed to `PlanResourceChange` as
returned. Only values cuenv builds itself (configuration and proposed new
state) are encoded by cuenv.

### Terraform core lifecycle, per resource

1. Refresh: `UpgradeResourceState` (stored JSON and schema version; the
   upgraded JSON form is used and a null upgraded state is refused) then
   `ReadResource`, whose MessagePack bytes become the prior state for plan and
   delete.
2. Plan: `ValidateResourceConfig`, then `PlanResourceChange` with a proposed
   new state from a port of Terraform's `objchange.ProposedNew` (nested
   collections, set matching, `optionalValueNotComputable`). The planned state
   is checked by a port of `AssertPlanValid` (`crates/infrastructure/src/object_change.rs`),
   recursing through nested attributes and blocks; a null planned state is
   always refused. Legacy SDK providers get Terraform's tolerance, logged at
   debug level only. `requires_replace` paths are kept only when the value at
   that path differs by schema type; a path found in neither value is a
   provider error. Non-empty results become destroy-then-create.
3. Apply: `ApplyResourceChange`; the resulting state is written immediately.
   Following Terraform's apply-result rules: a failed step that returns an
   object keeps the stored taint and dependencies; a create that fails or
   returns unknown values is tainted; a delete that returns an object without
   errors is an error and the object stays recorded; a create or update that
   returns null without errors is an error and leaves state untouched. Create
   and update results without errors are checked against the plan by a port
   of `AssertObjectCompatible`; an inconsistent result is an error, and a
   create is recorded tainted.
4. Delete: providers that advertise the `plan_destroy` capability are asked
   to plan every delete (destroy mode and orphans) with a null configuration;
   their errors, deferrals and non-null plans refuse the delete, and the
   planned private data reaches `ApplyResourceChange`. This is how provider
   deletion protection works.

Stored state whose schema version is newer than the provider's is refused
before the provider sees it (a downgraded provider would silently drop
attributes). Values a provider returns as JSON are converted to MessagePack
(lossless, since JSON cannot carry unknown values); values in `dynamic`
slots keep their concrete type through MessagePack, JSON and stored state, and
types are inferred only for CUE configuration. Set-typed configuration is
deduplicated, as Terraform's conversion does. A plan is checked against the
store at the start of `apply`, so a stale or already applied plan is refused.

Declared resources are ordered by `dependsOn`; orphans (recorded but no
longer declared) are deleted first in reverse dependency order.

### State: one Turso row per managed resource, keyed by tenant

```sql
cuenv_infrastructure_resources(
  module_path, project, resource_type, resource_name,   -- primary key
  provider, provider_source, schema_version,
  state_json, private, dependencies_json, serial, created_at, updated_at)

cuenv_infrastructure_locks(module_path, project, lock_identifier, holder, acquired_at)

cuenv_infrastructure_owners(module_path, project, instance, claimed_at)
```

- The owner row records which CUE instance (`<directory relative to the module
  root>:<package>`) manages the tenant. The first locked run claims it; any
  other instance is refused until `cuenv infrastructure state adopt` transfers
  ownership explicitly. This is the fence against two instances sharing a
  project name where module discovery cannot see both (directories the CUE
  loader skips, nested modules, other checkouts).

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

### Named environments and project secrets

The global `--env NAME` selects a complete provider/resource configuration at
`infrastructure.environments.NAME` and the matching project values at
`env.environment.NAME`. The common `infrastructure.state` backend is outside
that selection. No-flag use retains the original top-level configuration and
legacy state identity; an explicit name, including `default`, is a distinct
identity. Unknown or incomplete selections fail during CUE evaluation before
state access or provider startup.

Schema v4 preserves the original resource, lock and owner tables for legacy
no-flag runs. It creates a separate named-environment table family, keyed by
module, project, environment and resource address. Every named read, write,
lock, owner transfer, recovery hash and plan digest uses that identity, with no
fallback between table families. This avoids changing the key beneath old
clients that may still be running; reads do not migrate, and the first write
creates or upgrades the tables under the existing migration gate.

Schema v5 adds a generation UUID to both resource table families and backfills
existing rows. Updates retain the generation; deletion and recreation allocate
a fresh one. Plans compare it as part of their stored basis. Conditional create
writes retain the UUID from their write payload, and recovery file format v4
saves it along with the expected generation and serial, so a lost-response retry
cannot acknowledge an independent insertion with identical content. Format-v2/v3
files remain readable and require explicit force after inspection, with a CLI
warning: v2 lacks a generation, while v2/v3 lack the v4 backend binding. The
backend binding hashes the validated normalized URL and is checked against
both the recovery wrapper and actual state store before any file is written.

Ordinary Project decoding keeps infrastructure as raw JSON. Only infrastructure
commands deserialize the strict selected configuration; unused incomplete named
environments do not break task discovery, sync or CI.

Apply separates replacement phases: old replacements and their removed
dependents are deleted in reverse stored dependency order, then desired objects
converge in forward configuration order. Other orphan deletes remain last so
an in-place dependent update can detach before its old parent is removed.
Each provider result is recorded before proceeding. A failed dependent delete
leaves its parent intact, and a stopped run can resume from an absent replacement.

Plan, apply and destroy resolve the selected project environment through
Cuenv's existing secret resolvers. State-only commands resolve only the
configured backend token, so unavailable provider credentials do not prevent
state inspection, recovery or unlocking. `allowInfrastructure` filters before
secret retrieval and can name `plan`, `apply`, `destroy`, `state-list`,
`state-remove`, `state-recover`, `state-adopt` or `unlock`. Resolved secret
parts are registered for redaction before state and provider work. Providers
inherit ambient variables for compatibility, then receive resolved project
values and their plugin variables; policy-denied project names and the state
authentication token are removed before launch. If the project does not
declare the state-token variable, the existing ambient-token path remains.

The plan digest includes the named tenant and a process-salted fingerprint of
resolved project provider variables. This binds a plan to the selected
environment without writing those values or a reusable plain hash into output.

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
alias (`randomProvider`). The bridge now fails evaluation instead: the
infrastructure command requires the block to be concrete (`concretePaths`).

## Review practice

Every milestone gets an adversarial review from five personas in parallel —
security engineer, platform operator, Terraform protocol specialist, CUE and
schema designer, cuenv maintainer — with each finding verified before it is
acted on. The persona briefs live in `.agents/skills/cuenv-infrastructure/SKILL.md`.

### Milestone 1 review (proof of concept) and the hardening it drove

About fifty findings, most reproduced. Resolved in milestone 2:

- Security: version and source validation with cache containment (a crafted
  version could delete directories outside the cache); cached binaries
  re-verified from a manifest; HTTPS-only downloads; extraction permission
  and size limits; the state token withheld from providers, redacted
  everywhere and refused over non-loopback plaintext; errors name value kinds,
  never values; nested sensitive values masked.
- Operations: duplicate project names refused (two projects could delete each
  other's resources); lock-fenced writes; unlock by identifier; retries with
  lock-acquisition recovery; versioned migrations; unrecordable changes saved
  locally; failed creates tainted; plan, confirm, lock, re-plan; interrupts
  finish the resource in flight and release the lock; provider logs attached
  to failures; per-provider socket directories removed; graceful provider
  shutdown; dedicated error category and exit codes.
- Protocol: failed applies saved with unknowns as null; refreshed state kept
  current for unchanged resources; `requires_replace` filtered to changed
  paths; basic plan validity checks (null plans, non-computed attribute
  drift) honouring `legacy_type_system`; JSON-encoded values decoded;
  deferrals rejected; replacement creates receive the first plan's private
  data; unknown values from refresh rejected.
- CUE and runtime validation: strict version strings, valid token variable
  names, well-formed resource types, and typed/closed provider and resource
  configuration are schema-checked. The Rust engine enforces exactly one of
  `version`/`path` and rejects unknown provider/resource references before
  launching a provider. The Rust Turso parser checks loopback-only plaintext
  URLs and emits fixed errors without echoing the URL. The `infrastructure`
  block must be concrete (generic `concretePaths` bridge option, which fails
  closed on a missing or malformed path and is applied only by the
  infrastructure command).

### Milestone 2 review (hardening) and the decisions it drove

The five personas re-ran against the hardened build with fault-injecting proxies,
a fake provider and signal scripts. Half of the milestone 1 findings were fully
fixed; the rest were partly fixed, and new findings clustered in four places:

- **Interrupts.** The first interrupt only stopped between resources, so a
  continuous-integration cancel (SIGINT, then SIGTERM about 7.5 seconds later)
  force-exited while a create was in flight: the provider kept running, the
  resource went unrecorded and the lock leaked. Decision: follow Terraform. The
  first SIGINT or SIGTERM asks every running provider to stop and records what
  comes back; the second kills providers synchronously, releases the lock within
  a bound and prints the lock identifier with a direct write. The command owns
  interrupts from before it takes the lock.
- **Unrecorded changes.** The fallback file lived in the project tree (where
  continuous-integration workspaces are uploaded or discarded), nothing read it
  back, and when it could not be written the error printed the full state,
  secrets included. Decision: the file moves to the user state directory, errors
  never carry state, `cuenv i state recover` records it under the lock, and
  `plan`, `apply` and `destroy` refuse to run while such files exist.
- **Tenancy.** The duplicate-name check covered only the selected CUE package and
  skipped instances that failed to evaluate, so a same-named project in another
  package planned to delete the first project's resources. The generic
  `concretePaths` option was also applied to every command's workspace
  evaluation, silently dropping instances for other commands. Decision: check
  every package and fail closed; apply `concretePaths` only to the
  infrastructure command's own evaluation.
- **Protocol fidelity.** Plan validity rejected valid Plugin Framework plans for
  nested attributes with computed children; a JSON-encoded planned state became
  a null (turning an update into a delete); taint was lost when the delete half
  of a tainted replacement failed; the prior state was re-encoded lossily.

Also decided: `apply --yes` applies the plan made under the lock (no preview);
interactive confirmation compares a digest of every change rather than address
and action; refresh-only records count as work; JSON mode emits exactly one
envelope on standard output; `state` gains `list`, `remove` and `recover`.

Rejected, with reasons:

- Adding the CUE package to the tenant key. The tenant is the module path and the
  discriminator is the project, by requirement; the fail-closed duplicate check is
  the fence.
- Database triggers that fence older cuenv binaries out of upgraded tables. No
  released binary has written these tables; revisit with the first release.
- Treating the new public `concrete_paths` field as a semantic-versioning break.
  Workspace crates are versioned together.
- Removing the always-empty `identity` column. It is reserved for resource
  identity (next step 2).

### Milestone 3 review (after the milestone 2 fixes)

The same five personas re-ran against the fixed build. Nothing corrupted
state, but five findings were serious:

- **Silent typos.** Projects embed `schema.#Project` at file level, which
  bypasses closedness for the `infrastructure` block, so `resource:` instead of
  `resources:` planned the deletion of everything. Fixed on both sides: the
  schema wraps definition references so closedness holds, and the Rust types
  reject unknown fields.
- **Tenancy through skipped directories.** The CUE loader's module walk skips
  directories starting with `_` or `.`, `testdata` and nested modules, yet
  `--path` can target them, so the fail-closed duplicate check could not see
  them. Fixed with the owner record in the state store (see State) and an
  explicit refusal when the target is invisible to the walk.
- **Destroy bypassed deletion protection.** `plan_destroy` was not honoured.
- **Apply results were not checked** against the plan (`AssertObjectCompatible`).
- **`state recover` overwrote newer state.** Unrecorded files now record the
  version they replace, and recovery is a compare-and-swap (`--force` to
  override).

Also fixed: dynamic values keep their types; newer stored schema versions are
refused; set configuration is deduplicated; a replacement honours a stop
between its halves; provider process groups are killed as a whole, and on
Linux providers die with cuenv; SIGHUP and SIGQUIT are handled like SIGTERM;
socket directories are removed on a forced exit; the unrecorded store refuses
symbolic links and foreign owners and writes atomically; store errors never
carry request or response bodies; control characters are stripped from
provider output and database strings; the registry client is HTTPS-only
across redirects; JSON mode keeps its one-document contract on every exit
path.

Decision reversed: interactive `apply` now holds the lock from planning
through the confirmation prompt, as Terraform does, instead of re-planning
under the lock and comparing digests. The re-plan could never be both safe and
stable (volatile attributes changed the digest on every run). The plan digest remains in the library, where `apply` uses the plan's
stored-record basis to refuse a stale or already applied plan.

Deferred: the Nix checks that would run the fake-provider suite and the
bridge's `go test` in continuous integration (written once the local Nix gate
works again), and moving older cuenv-specific helpers (`injectTaskNames`,
`isProject`, `Projects`) out of cuengine, which predates this work and touches
every command.

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
- A Plugin Framework fake provider (`crates/infrastructure/tests/fake_provider`,
  Go; `go build -o terraform-provider-fake .`, then set
  `CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER`) reproduces the milestone 2
  protocol findings: nested computed children, JSON-encoded planned state,
  taint on failed replacements, a delete that returns an object, semantic
  equality, nested `dynamic` wrappers, and slow creates for stop, kill and
  process-group checks. It also enforces dependent-before-parent deletion and
  parent-before-dependent creation, including failed and interrupted deletes.
  Protocol regression tests were confirmed to fail against the old
  behaviour.

## Next steps

1. Cross-resource references. The real value of CUE here: let a resource's
   configuration reference another's attributes, propagate unknowns through
   planning, and resolve them during apply in dependency order. Derive
   `dependsOn` from references.
2. Protocol fidelity: resource identity (`GetResourceIdentitySchemas` and
   identity on read, plan and apply); numbers with full precision beyond 64-bit
   floats and integers; masking by sensitive path rather than whole top-level
   attribute.
3. Tests that run in continuous integration: build the fake provider and run
   it with a mock Hrana server in a Nix check, so the lifecycle suite no longer
   needs real binaries or is ignored; add `go test` for the cuengine bridge,
   which no check runs today.
4. A typed provider binding from github.com/cuenv/terraform (for example a
   generated `#Provider` carrying `source`, `version` and a resource-type to
   definition map) so `type`, `version` and `configuration` cannot disagree.
   Needs a decision across both repositories.
5. Data sources (`ReadDataSource`) and imports (`ImportResourceState`),
   `--target`.
6. Parallel apply across independent resources.
7. Provider version constraints, lock file entries in `cuenv.lock`, and GPG
   verification of `SHA256SUMS`. Until then the cache manifest only detects
   accidental corruption: anyone who can write to the plugin cache can replace
   both a binary and its manifest.
8. Secret-typed provider and resource arguments resolved through cuenv's
   secret resolvers instead of plaintext CUE. Provider environment values and
   the state token already use the selected project environment, existing
   resolvers and `allowInfrastructure`; the token remains configured by
   environment-variable name.
9. Lock leases with expiry and heartbeat instead of manual release, and a
   `--lock-timeout` that waits for a running apply instead of failing
   immediately.
10. Cancellable evaluation: the first interrupt during CUE evaluation
    currently abandons the evaluation thread rather than stopping it.
11. A fenced "pending" record written before each create, so a run killed
    outright (SIGKILL, host loss) leaves evidence of what may exist.
12. In github.com/cuenv/terraform, `#ProviderConfig` abbreviates
    "configuration"; renaming it to `#ProviderConfiguration` would bring the
    generated modules in line with the no-abbreviation rule.
