# Infrastructure as Code through Terraform Provider Plugins (Proof of Concept)

Status: **experimental** proof of concept, implemented in
`crates/infrastructure` (`cuenv-infrastructure`) and `cuenv infrastructure`
(short form `cuenv i`). The command is marked experimental in `--help` and in
every document: its behavior, flags, schema and state layout can still change
between releases (decision D7 below).

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

Operations are ordered as one dependency graph, with the edges of Terraform's
`DestroyEdgeTransformer` (see "Apply order" below).

### State: one Turso row per managed resource, keyed by identity

```sql
cuenv_infrastructure_resources(
  module_path, project, environment, resource_type, resource_name,   -- primary key
  provider, provider_source, schema_version, state_json, private,
  dependencies_json, tainted, identity_json, serial, generation,
  created_at, updated_at)

cuenv_infrastructure_locks(module_path, project, environment, lock_identifier, holder, acquired_at)

cuenv_infrastructure_owners(module_path, project, environment, instance, claimed_at)

cuenv_infrastructure_schema(version)
```

Schema version 1 is the only layout. `environment` is the empty string for a
run without `--env`; a named environment can never be empty, so the no-flag
identity and each named environment are separate tenants that never read each
other's rows, locks or owners, and nothing falls back from one to the other.
Rows carry a generation UUID (kept on updates, replaced when an address is
deleted and created again) and a serial; plans compare both as their stored
basis, conditional create retries retain the UUID of their payload, and
recovery saves both, so a lost-response retry cannot acknowledge an independent
insertion with identical content. `identity_json` is reserved for resource
identity (next step 2).

The schema is versioned (`cuenv_infrastructure_schema`): a newer version than
the build knows is refused for reads, locks and force-unlock
(`StateSchemaNewer`), reads never migrate, and each migration runs in its own
`BEGIN IMMEDIATE` transaction. A migration after the first refuses, inside its
transaction, while any lock row exists (`StateMigrationBlocked`), and the lock
insert is guarded by the schema version in the same statement. See decision D1
for the reasons, and its consequence: one stale lock blocks every future
migration until it is released. A row that cannot be decoded is
`UndecodableRecord` naming the address, not a Turso connectivity error.

- The owner row records which CUE instance (`<directory relative to the module
root>:<package>`) manages the tenant. The first locked run claims it; any
  other instance is refused until `cuenv infrastructure state adopt` transfers
  ownership explicitly. This is the fence against two instances sharing a
  project name where module discovery cannot see both (directories the CUE
  loader skips, nested modules, other checkouts).

- The tenant is the CUE module path from `cue.mod/module.cue` (major-version
  suffix stripped); the project name discriminates within it, and the selected
  environment (empty without `--env`) completes the identity. `TenantKey`
  cannot be constructed without the first two, and every `StateStore` method
  takes one.
- Tenancy is a naming boundary, not a security boundary: the module path is
  declared by the project, so anyone with the database token can reach every
  tenant and every environment. Separate databases per tenant give real
  isolation.
- Rows store `cty` JSON, exactly what `UpgradeResourceState` consumes, so state
  written by one provider version is upgraded by the next.
- The store speaks Hrana over HTTP (`POST /v2/pipeline`) with reqwest, which
  works for Turso Cloud and self-hosted `sqld` without a native libSQL
  dependency.
- Locks are `INSERT … ON CONFLICT DO NOTHING`; zero affected rows means
  another holder, which is reported with its identity.

### Named environments and project secrets

The global `--env NAME` selects a complete provider/resource configuration at
`infrastructure.environments.NAME` (including its `providerEnvironment`) and the
matching project values at `env.environment.NAME`. Nothing is inherited or
merged from the top level. The common `infrastructure.state` backend is outside
that selection. No-flag use retains the top-level configuration and the
no-flag state identity; an explicit name, including `default`, is a distinct
identity. Unknown or incomplete selections fail during CUE evaluation before
state access or provider startup, and the error lists the declared names.

Every named read, write, lock, owner transfer, recovery hash and plan digest
uses the full identity (module, project, environment). Ordinary Project
decoding keeps infrastructure as raw JSON; `Infrastructure::select` strictly
decodes only the selected configuration, so unused incomplete environments do
not break task discovery, sync or CI.

**One state database.** All environments share the one `infrastructure.state`
backend and token (decision D4). It is documented in the how-to and the schema
reference; per-environment `state` is a follow-up.

**No-flag to `--env` is refused, not migrated** (decision D3). `plan`, `apply`
and `destroy` with `--env NAME` refuse when `NAME` has no records while the
same module and project have no-flag records, and a no-flag run refuses when
`infrastructure.environments` is declared and top-level `resources` is absent.
Removing or renaming an environment strands its state (`--env OLD` can no
longer select a configuration), so the documentation says to destroy first; the
state commands need only `infrastructure.state` and keep working for an
undeclared environment, with a warning. Every hint cuenv prints carries the
run's `--env`, `-p` and `--package`.

**Apply order.** Apply is one dependency graph, not phases. The plan's changes
become operations (a replacement is a delete node and a create node) with the
edges of Terraform's `DestroyEdgeTransformer`:

- creates and updates wait for the creates and updates of their configured
  prerequisites;
- a delete waits for the deletes of everything whose **stored** record depends
  on it, and a create or update waits for the deletes of everything whose
  stored record depends on it (Terraform's creators edge);
- two preferences are added only while they cannot close a cycle: an in-place
  update that detaches from a parent runs before the delete of its old parent,
  and a replacement's delete waits for the creates and updates its replacement
  depends on, so a failed prerequisite is known before the old object is
  destroyed;
- among runnable operations deletes go first, then refreshes, then creates and
  updates, each group in dependency order; a replacement's create follows its
  delete at once.

The schedule is computed in `plan()`: cycles are refused before any
confirmation, naming the operations, and the plan lists its changes in apply
order so the preview is the order of events. Dependencies are stored as full
`type.name` addresses (bare names in old records resolve against the tenant's
stored records), which removes a false cycle between two resources named alike.
The delete half of a replacement is sent the change's planned private data, as
in Terraform. On a provider failure the operations that depend on it are
skipped and the rest still run; a replacement whose create can no longer run is
not started, and every replacement whose old object was deleted but not
recreated is reported on every ending (`ApplyEvent::DeletedNotRecreated`, the
error's help text and `deletedNotRecreated` in the JSON error envelope).
`apply` refuses a plan made with a different provider environment
(`PlanEnvironmentChanged`). Each provider result is recorded before proceeding.

**Secrets.** Plan, apply and destroy resolve the selected project environment
through cuenv's existing secret resolvers. State-only commands resolve only the
configured backend token, so unavailable provider credentials do not prevent
state inspection, recovery or unlocking. `allowInfrastructure` filters before
secret retrieval and takes the `#InfrastructureAction` names `plan`, `apply`,
`destroy`, `state-list`, `state-remove`, `state-recover`, `state-adopt` and
`unlock` (anything else fails evaluation and deserialization); a variable with
policies is available only when some policy lists the running action, so a
variable that carried only `allowTasks` is now withheld from infrastructure
commands. The documented `{value, policies}` form works because `#Secret`
forbids `value` and `policies`: before, the open `#Secret` also matched it, which
made the `#EnvironmentVariable` disjunction ambiguous and left the variable
unresolved (for `allowTasks` as well).

**Redaction.** Resolved secret parts and the state token are registered before
state and provider work, together with the values of cuenv's own resolver
credentials found in its environment at startup (the table in
`cuenv_secrets::credentials`, which also drives withholding). Registration
includes each line of a multi-line secret, the debug- and JSON-quoted forms of a
secret with escapable characters, and Go's JSON form (`&`, `<` and `>` written
as `\u0026`, `\u003c` and `\u003e`, which Terraform providers log with).
Every event is redacted in `emit_with_source` before any subscriber sees it (so
`emit_stdout!` and `emit_stderr!` text is covered) and again by the CLI and JSON
renderers. Events are redacted by type, field by field: no secret, whatever it
equals (`data`, `type`, `content`), can rename a tag or key, change a variant or
withhold an event. Other JSON is redacted string by string, never as serialized
text, and its object keys are kept unless the JSON is free-form (provider logs),
where keys are content too. A provider's JSON log line is parsed and each decoded
string redacted, so Go's escapes cannot hide a secret. `RedactingStderr`
redacts tracing's formatting layers; `CliError` redacts its message and help when
it is built (so terminal wrapping cannot split a secret) and again when shown;
provider log lines and gRPC messages are redacted before control characters are
stripped. A failing secret command's error output is never quoted, because
nothing is registered for redaction until a secret has resolved; the same holds
for the 1Password, AWS and Google Cloud command-line resolvers, which report the
exit status only.

The registry compiles its secrets into one Aho-Corasick matcher the first time
text is redacted after the registry changed (a generation counter detects
that), and every redaction is a single pass that replaces the union of all
matches, overlapping ones included. The cost of redacting a string no longer
grows with the number of registered secrets, and nothing is cloned or sorted per
call. The provider log drain classifies each line first and redacts only the
lines it keeps (warnings, errors, and anything that is not hclog JSON), so the
trace-level chatter of a large plan is never redacted at all.

**Provider environment** (decision D5). `providerEnvironment` (top level and per
environment, no inheritance) is `inherit` (default) or `isolated`. A selected
environment that does not set it while the top level does is refused (explicit is
safer than a silent fall back to `inherit`; the key's presence is read from the
raw configuration, not from a defaulted value). In `inherit`
mode providers keep the ambient environment, minus the variables of cuenv's
secret machinery (the table `cuenv_secrets::RESOLVER_ENVIRONMENT_VARIABLES`,
exact names and prefixes: `OP_SERVICE_ACCOUNT_TOKEN`, `OP_CONNECT_TOKEN`,
`OP_SESSION_*`, `OP_CONNECT_HOST` (withheld, not redacted),
`INFISICAL_TOKEN`, `INFISICAL_CLIENT_ID`, `INFISICAL_CLIENT_SECRET`,
`VAULT_TOKEN`, `CUENV_SECRET_SALT`, `CUENV_SECRET_SALT_PREV`), the variables the
project's `cache.remote.auth` names, and the state token, unless the
project passes a variable of that name. In `isolated` mode the environment is
cleared except `PATH`, `HOME`, `USER`, `LOGNAME`, `TMPDIR`, proxy and TLS
variables; proxy URLs lose their userinfo (credentials the provider was not given
on purpose; an authenticated proxy is passed explicitly in `env`). Variables with
names that are not valid unicode are never passed in either mode. In both modes the project's policy-allowed values are added last,
policy-denied project names are never inherited from the host, and cuenv's own
handshake variables are set after the withheld names are removed, so a policy
cannot strip them.

This is hygiene, not a sandbox. A provider runs as the user: `HOME` is passed even
in `isolated` mode, so `~/.aws`, `~/.vault-token`, `~/.config/op` and
`credentials.tfrc.json` are readable, and every provider receives every
policy-allowed project variable (there is no per-provider scoping). The kernel
would also have let a provider read cuenv's own `/proc/<pid>/environ`, which holds
everything that was withheld, so on Linux cuenv calls `prctl(PR_SET_DUMPABLE, 0)`
first thing in `main` (a same-user child then gets permission denied; `execve`
resets the flag for the programs cuenv starts; a debugger cannot attach and no
core dump is written; `/proc/<pid>/cmdline`, which cuenv's own process discovery
reads, stays readable). There is no equivalent on macOS. Untrusted providers need
operating-system isolation (a container, a virtual machine, a separate user).

The plan digest includes the identity and a process-salted fingerprint of
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
  launching a provider (milestone 4 restored the same checks in CUE; the Rust
  checks remain as defense in depth). The Rust Turso parser checks loopback-only plaintext
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
works again; done in the milestone 4 round, see below), and moving older
cuenv-specific helpers (`injectTaskNames`, `isProject`, `Projects`) out of
cuengine, which predates this work and touches every command (still open; see
next steps).

### Milestone 4 review (after the milestone 3 fixes) and the decisions it drove

The five personas re-ran against pull request head `ba11c96` (diff
`92409ba..ba11c96`). There were no regressions of the milestone 2 and 3 findings
the personas retested, and several things were confirmed sound: closedness
including `environments`, the selection semantics, policy filtering before
secret resolution, the lost-response compare-and-swap with generations, tenant
isolation per environment, signal handling, one JSON document per command and a
3000-resource dependency chain planning in 1.8 seconds. The confirmed findings
clustered in six places. This section records them and how the fix round
resolved each.

**1. Apply order destroyed objects (protocol P1 to P6, operator R1, N4, N6).**
Apply ran a phase schedule: every replacement delete ahead of all forward work,
orphan deletes last. An unrelated failure left replaced resources destroyed and
never recreated, with no warning; renaming a resource key with the same
real-world identity created first and then deleted the object both records
managed (`local_file` lost its file and the run reported success; unique
identity resources failed forever); a removed child was deleted after its
parent's update, so updates that need the child gone failed on every run;
ordering refusals appeared only after confirmation; stored dependencies were bare
names, so a type change made a false cycle. _Resolved:_ one dependency graph
with Terraform's destroy edges (see "Apply order"), computed in `plan()`, with
full-address dependencies, failure that skips only dependents, and reporting of
every replacement deleted but not recreated, including `deletedNotRecreated` in
the JSON envelope. Regression tests: scheduling units, a scripted runner with the
in-memory store, fake-provider end-to-end cases and a real `local_file` rename.
The reason for a graph rather than patching the phases: the phases were the
cause of three separate findings that contradicted each other (the specification
described both delete-first and delete-last), and Terraform's rules are a known,
tested answer.

**2. Secrets reached output (security S1, S2, S4, S7).** `emit_stdout!` and
`emit_stderr!` output was never redacted, so a plan printed a resolved secret in
clear; error redaction ran after miette wrapped the text, so a long secret split
across lines, and JSON escaped forms leaked; provider log lines were redacted
after control characters were stripped; every ambient resolver credential
(`OP_SERVICE_ACCOUNT_TOKEN`, `VAULT_TOKEN`, ...) reached every provider; a
failing secret command's standard error was printed before anything was
registered. _Resolved:_ redaction at event emission and at error construction,
per string for JSON, line by line for multi-line secrets, in quoted forms, for
tracing layers and for provider output (see "Redaction"); resolver credentials are
withheld from providers and an `isolated` mode exists (decision D5); the secret
command's error output is no longer quoted.

**3. The schema checks had moved to Rust for a wrong reason (schema 1 to 5, K-4,
K-7).** The semantic CUE checks were removed in `ba11c96` on the claim that
`error()` breaks consumers on CUE language v0.9. That was false: `error()` is
gated by the language version of the module that holds the file, so registry
consumers on v0.9 are unaffected (verified with a v0.9 consumer of a published
copy); only in-module copies, such as a test fixture, need v0.14. Separately, the
documented `allowInfrastructure` snippet did not deserialize, a misspelled
`allowInfrastucture` passed silently, `allowInfrastructure` accepted any string,
the Rust checks stopped at the first error without a path, `path: ""` was
accepted when the schema was not embedded, and `--env` errors named the wrong
path. _Resolved:_ decision D2, the `#Secret` and `#Policy` fixes, the
`#InfrastructureAction` enum, and error messages that name the real
configuration path and say that top-level providers are not inherited.

**4. State identities and stranded state (operator N1 to N3, N5, N7, R2, schema 3,
6, 7).** An old client mid-run during the generation migration could leave the
whole tenant unreadable; switching a project from no flag to `--env` silently
recreated everything and both identities claimed the same objects; a project with
only `environments` run without `--env` exited 0 with "no changes" or planned to
delete the no-flag state; hints dropped `--env`, so `recover` without it exited 0
doing nothing and `unlock <id>` exited 0 with the lock still held; removing an
environment stranded its state; v2 recovery files could be recorded only with
`--force`, which also dropped the compare-and-swap for newer files; the backend
binding depended on how a loopback URL was spelled. _Resolved:_ decisions D1, D3,
D4, with notes naming other environments and every hint carrying `--env`, `-p`
and `--package`.

**5. Delivery (maintainer B1 to B6, F1 to F12).** Thirty-two new ignored tests
ran nowhere in continuous integration; the bridge's `go test` never ran either
(the `cue-bridge` derivation overrides `buildPhase`, so the inherited
`checkPhase` failed on `getGoDirs`, silently); the reference documentation was
stale; the pull request was 98 files and 35 commits with an empty body;
`exportsTasks` hard-coded `tasks` in cuengine; macOS was never compiled even
though releases build `macos-arm64`; the command was not marked experimental and
is not behind a cargo feature. _Resolved:_ an explicit Go `checkPhase`; the
`cuenv-fake-terraform-provider` and `cuenv-infrastructure-e2e` flake checks
(Linux, with `sqld`, the nixpkgs `random`, `local` and `tfe` providers and the
fake provider), wired into the CI pipeline; a named-environment lifecycle test
with a secret released through `allowInfrastructure`; the `task_field` bridge
option; rewritten reference documentation, matrix and skill; the
`.claude/worktrees/` ignore rule; the Turso unit tests moved out of the 3.6
thousand line `turso.rs`. Not resolved: see next steps.

#### Decisions of the fix round

- **D1. Collapse pre-release persistence.** No released cuenv ever wrote the
  state tables or the recovery files. The five-step migration history, the
  legacy and named table families and the v2 and v3 recovery readers protected
  databases that exist only in development, and they were the source of
  findings N1 (a half-migrated database unreadable), R2 (force dropping
  compare-and-swap) and N7. They are replaced by one schema version 1 and one
  recovery format version 1. The versioned framework stays (schema table, newer
  versions refused, fail closed) because the first released layout will need to
  evolve, and future migrations are fenced: a migration refuses to run while any
  lock row exists, checked inside its transaction, because a run that holds a lock
  may be between two writes. Consequences: one stale lock blocks the next schema
  migration until released; and development databases from earlier iterations of
  this pull request are not adopted, so their `cuenv_infrastructure_` tables must
  be dropped.
- **D2. Restore the CUE semantic checks.** The reason given for removing them was
  wrong (see above). They return as `error()` calls scoped so each applies to the
  top level and to each `environments.NAME` configuration, each against its own
  maps, in linear time. Test fixtures that copy the schema into a module declare
  language v0.14.1. The Rust checks remain as defense in depth for projects that
  do not unify with the schema and now collect every problem with its field path.
  Consequence, deliberately fail-closed: a semantic error in any environment,
  selected or not, is a CUE evaluation error and fails every command that
  evaluates the project; state-only commands still fail while the project does not
  evaluate and need the configuration fixed first, even though they need only
  `infrastructure.state`.
- **D3. Refuse no-flag to `--env` instead of migrating.** Moving rows and the
  owner between identities needs both locks, an ownership decision and a rollback
  story, and the alternatives were worse: silently recreating every resource
  beside the objects the old state still manages. Refusing is safe now, and
  `state move` is a follow-up. A no-flag run is also refused when only
  `environments` is declared.
- **D4. One state database for all environments.** A per-environment `state`
  would change the identity, the token and the URL validation of every command,
  including the state-only ones that must work when the rest of the configuration
  is broken. It is documented instead, and is a follow-up.
- **D5. Provider environment.** Ambient inheritance stays the default because the
  AWS, Google and other providers read ambient cloud credentials and would break;
  those credentials (`AWS_*`, `GOOGLE_APPLICATION_CREDENTIALS`) therefore remain
  inherited in `inherit` mode. What providers never needed are the keys to
  cuenv's own secret stores, which are always withheld unless the project passes
  them. An opt-in `isolated` mode covers the rest. Residual exposure (a continuous
  integration runner's OIDC token, `GITHUB_TOKEN`) is documented, not hidden.
- **D6. Packaging.** The pull request is restructured into stacked pull requests
  at the end; the fix round only commits.
- **D7. Mark the command experimental.** `--help` and every document say so. It
  is not behind a cargo feature this round, because that decision (it would keep
  tonic and the other dependencies out of every other build) changes the build and
  release surface; it is a follow-up.
- **Redaction at the source.** Events are redacted where they are emitted and
  errors where they are built, not where they are rendered, because renderers
  cannot know what a later layer will wrap or escape.
- **State-only commands export only `infrastructure.state`.** Incomplete
  providers and resources never keep state from being listed, unlocked or removed;
  the declared environments come from a second, best-effort evaluation.
- **The bridge takes the task graph field from the caller** (`task_field`), so the
  new `exportsField` helper carries no cuenv name; existing callers keep `tasks`.

## Validation

- Unit tests: `cty` codec and set equality, schema conversion, normalization
  and proposed state, handshake parsing, procedure naming, Hrana wire format,
  tenant parsing, memory store isolation and locking, registry cache layout and
  archive extraction, diagnostics, the apply schedule (including cycles, renames
  and detachment) and running it with a scripted runner, validation with field
  paths, provider environment, redaction and plan rendering.
- Ignored integration tests (`crates/infrastructure/tests/provider_end_to_end.rs`)
  run the full lifecycle — create, idempotent re-plan, forced replacement,
  orphan delete, destroy, tenant isolation, rename — against real
  `hashicorp/random` and `hashicorp/local` binaries with both the in-memory store
  and `sqld`; load a protocol 6 schema from `hashicorp/tfe`; and install from the
  live registry. The Turso module tests include the migration fence against a
  real `sqld`, and `crates/cuenv/tests/infrastructure_lifecycle.rs` runs the CLI
  against a real provider: a no-flag stack, and a named `--env` stack whose
  provider receives an exec secret through `allowInfrastructure` (it arrives, is
  not stored in state and never appears in output).
- A Plugin Framework fake provider (`crates/infrastructure/tests/fake_provider`,
  Go; `go build -o terraform-provider-fake .`, then set
  `CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER`) reproduces the milestone 2
  protocol findings: nested computed children, JSON-encoded planned state,
  taint on failed replacements, a delete that returns an object, semantic
  equality, nested `dynamic` wrappers, and slow creates for stop, kill and
  process-group checks. It also enforces dependent-before-parent deletion and
  parent-before-dependent creation, including failed and interrupted deletes,
  and, since milestone 4, objects with unique identities (`fake_obj`,
  `fake_obj2`) for the ordering regressions. Protocol regression tests were
  confirmed to fail against the old behaviour.
- Continuous integration: the `cue-bridge` derivation has an explicit
  `checkPhase` (`go vet` and `go test` with the build's CGO toolchain), the
  `cuenv-fake-terraform-provider` check builds and vets the fake provider, and
  the Linux-only `cuenv-infrastructure-e2e` check runs the ignored suites against
  three `sqld` instances (shared, fresh and migration databases), the fake
  provider and the nixpkgs `random`, `local` and `tfe` providers, excluding the
  test that downloads from `registry.terraform.io`. It is wired into the pipeline
  as `checks.infrastructure-e2e`.

## Next steps

Resolved in the milestone 4 round and removed from this list: tests that run in
continuous integration (fake provider, `sqld`, the bridge's `go test`), the
spelled-out apply order, the CUE semantic checks, and marking the command
experimental.

1. Cross-resource references. The real value of CUE here: let a resource's
   configuration reference another's attributes, propagate unknowns through
   planning, and resolve them during apply in dependency order. Derive
   `dependsOn` from references.
2. Protocol fidelity: resource identity (`GetResourceIdentitySchemas` and
   identity on read, plan and apply); numbers with full precision beyond 64-bit
   floats and integers; masking by sensitive path rather than whole top-level
   attribute.
3. `cuenv infrastructure state move`: move records, and the owner, between the
   no-flag identity and a named environment, or between environment names, under
   both locks. Until then cuenv refuses the runs that would need it (decision D3).
4. Per-environment `state`: let `environments.NAME` override the state backend
   (and token) so environments can be isolated by credential, not only by name
   (decision D4).
5. A darwin continuous integration job. Releases build `macos-arm64`, but the
   `cfg(unix)` and Linux-specific code in `plugin.rs`, `holder.rs` and
   `unrecorded.rs` has never been compiled there, and the end-to-end check is
   Linux only.
6. Event redaction cost. Events are redacted by type with a compiled matcher (no
   JSON round trip, no per-call registry copy), but still in `emit_with_source`
   and again by each renderer. Measure it on task-heavy runs and, if it matters,
   redact once.
7. Narrow the public API before the crates are published to crates.io: every
   module of `cuenv-infrastructure` is `pub`, `StateStore` is unsealed, the
   recovery identity defaults to `None`, and registration, publish order and
   keywords need a decision.
8. Replace the remaining `unsafe` `libc` calls (process groups and parent-death
   signalling in `plugin.rs`, file ownership in `unrecorded.rs`, user, host name
   and account lookups in the command's `holder.rs`) with `rustix`.
9. The cuenv-specific semantics still in cuengine's task injection:
   `injectTaskNames`, `isTaskShaped`, `schemaPackagePath` and project detection
   remain hard-wired to cuenv's task and project shapes. Milestone 4 only made the
   field name (`exportsTasks`, now `exportsField`) a caller option (`task_field`);
   the rest should move out of the generic bridge. It predates this work and
   touches every command.
10. A cargo feature that keeps `cuenv infrastructure` and its dependencies (tonic,
    prost, rustls) out of builds that do not want them (decision D7).
11. A `validate`-style command or `state recover --dry-run` / `state show`, to
    inspect a saved recovery file and an environment's configuration without
    changing anything (operator N8).
12. A typed provider binding from github.com/cuenv/terraform (for example a
    generated `#Provider` carrying `source`, `version` and a resource-type to
    definition map) so `type`, `version` and `configuration` cannot disagree.
    Needs a decision across both repositories.
13. Data sources (`ReadDataSource`) and imports (`ImportResourceState`),
    `--target`.
14. Parallel apply across independent resources (the dependency graph already
    says which operations are independent).
15. Provider version constraints, lock file entries in `cuenv.lock`, and GPG
    verification of `SHA256SUMS`. Until then the cache manifest only detects
    accidental corruption: anyone who can write to the plugin cache can replace
    both a binary and its manifest.
16. Secret-typed provider and resource arguments resolved through cuenv's
    secret resolvers instead of plaintext CUE. Provider environment values and
    the state token already use the selected project environment, existing
    resolvers and `allowInfrastructure`; the token remains configured by
    environment-variable name.
17. Lock leases with expiry and heartbeat instead of manual release, and a
    `--lock-timeout` that waits for a running apply instead of failing
    immediately. Note that one stale lock also blocks future schema migrations.
18. Cancellable evaluation: the first interrupt during CUE evaluation
    currently abandons the evaluation thread rather than stopping it.
19. A fenced "pending" record written before each create, so a run killed
    outright (SIGKILL, host loss) leaves evidence of what may exist.
20. In github.com/cuenv/terraform, `#ProviderConfig` abbreviates
    "configuration"; renaming it to `#ProviderConfiguration` would bring the
    generated modules in line with the no-abbreviation rule.
21. Secrets are resolved before the owner check, so a run owned by another
    instance still resolves its secrets before being refused (security S7).
