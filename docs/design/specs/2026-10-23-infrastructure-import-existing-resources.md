# Importing Resources and Referencing Outputs

Status: **implemented**. `cuenv infrastructure` remains experimental. This
document records the behavior and first-release boundaries of import and
managed-resource output references.

## Goal

Bring a resource created outside cuenv under cuenv state management without
creating it again. This means asking the configured Terraform provider to
interpret its resource-specific import identifier, refreshing the returned
state from the real system, and recording that result for the selected cuenv
identity.

This is separate from `cuenv infrastructure state adopt`, which transfers
ownership of state already recorded by cuenv. Importing a user-supplied state
document is also outside this feature.

## Import command and behavior

```console
cuenv infrastructure import <type.name> <provider-import-identifier>
```

The existing global `--env NAME` selector chooses the complete infrastructure
configuration and state identity. The target must be declared in that selected
configuration so cuenv knows its resource type, provider, configuration,
dependencies, provider environment, and schema. The provider defines the
identifier format; cuenv passes it through unchanged and does not assume it is
an `id` attribute.

The command:

1. Evaluates the selected configuration, validates the target and provider, runs
   the whole-module project-name check, and resolve the configured provider
   environment and state backend.
2. Uses the existing provider installer, provider pin and schema-digest checks,
   state lock, ownership checks, cancellation handling, and provider shutdown.
3. Calls the provider's `ImportResourceState` operation with the target type and
   identifier. For protocol versions 5 and 6, the operation uses the paths
   `/tfplugin5.Provider/ImportResourceState` and
   `/tfplugin6.Provider/ImportResourceState`.
4. Requires exactly one returned resource of the requested type. Passes its
   partial state and private data to `ReadResource`; only the refreshed result
   is eligible for persistence. The command never calls
   `ApplyResourceChange` and never changes the remote object.
5. Refuses an occupied target address. Under the tenant lock, writes with an
   absent-record compare-and-swap so a concurrent or unexpected row cannot be
   overwritten. Uses the existing uncertain-write recovery path if the state
   store outcome is unknown.
6. Reports the imported address and the next environment-aware `plan` command.
   The next plan shows any difference between the refreshed object and its CUE
   configuration.

An import identifier can be sensitive. Register it for redaction before
provider calls, and keep it out of events, tracing fields, errors, and output.
Provider diagnostics and warnings continue through the existing redaction
path.

## First-release boundaries

- One configured target and one returned resource per command. Providers may
  return several resources for one identifier; reject zero or multiple results
  before writing anything. A later design would need an explicit mapping from
  each returned resource to a CUE name before multi-resource import is safe.
- The returned resource type must match the requested type. Reject provider
  errors, deferred responses, invalid encodings, a null root object, and
  unknown values in the final refreshed state. Null attributes are allowed
  when the provider cannot discover them; the normal plan can then report
  configuration differences.
- Require a string import identifier. Providers that return or require
  structured resource identity are refused until cuenv supports that identity
  through import, refresh, planning, and state persistence. Do not silently
  drop identity metadata.
- Require an existing CUE declaration. Generating configuration from imported
  state is a separate feature because state can omit write-only inputs and
  other values that cannot be reconstructed reliably.
- Import does not establish global uniqueness of a real object across
  projects, environments, or state databases. Operators remain responsible
  for binding a remote object to only one state address.

## Managed-resource output references

A top-level resource argument may use a descriptor with exactly two string
fields, `fromResource` and `attribute`:

```cue
configuration: {
	zone_id: {
		fromResource: "cloudflare_zone.primary"
		attribute: "id"
	}
}
```

`fromResource` names a declared resource by its full `type.name` address;
`attribute` names a top-level provider output. The source output and destination
argument must have the same primitive type (string, number, or boolean), and
the source output cannot be sensitive. Nested values, collections, unknown or
null final values, data sources, and references into undeclared resources are
not supported. References infer and persist the same dependency edges as
`dependsOn`.

When a producer's planned output is known, it participates in the consumer's
plan. If it is unknown until the producer is applied, cuenv resolves it and
replans the consumer immediately before applying it. If that final plan changes
the already-confirmed action into a replacement, cuenv stops before changing
the consumer; run `plan` again after the producer has been recorded. Generated
provider resource types accept the descriptor for top-level primitive
arguments.

## State, policy, and identity behavior

Import is a provider-using state mutation, not a state-only command. The
`import` value in `#InfrastructureAction` and `InfrastructurePolicyAction`
selects the full configuration and provider environment. A project variable
guarded by `allowInfrastructure` must explicitly permit `import` before the
variable is available to the provider.

Preserve the selected tenant exactly: CUE module path, project name, and
environment. There is no fallback between the no-flag identity and any named
environment. Reuse tenant ownership checks and the fenced state lock. The
configured target must be absent from that tenant; import must not replace an
existing record. Scope the existing cross-environment address guards to the
single import target, so unrelated configured resources do not block this
operation.

Persist the resource state as cty JSON with the selected provider name and
source, current resource schema version, provider private bytes, and the
configured full dependency addresses. Use the existing generation and serial
rules. A state write whose response is lost must produce a recovery record
with a stable generation, just like other resource writes.

## Implementation and review record

The feature landed as one integrated change across provider protocol, engine,
CLI, schema, tests, and documentation. The integration owner coordinated the
command contract, shared interfaces, and final diff across these scopes:

1. **Provider protocol — complete.** Added protocol version 5 and 6 message
   bindings, operation paths, provider client calls, focused wire/path tests,
   and fake-provider import results. Deferred and identity fields are decoded
   so unsupported results fail explicitly instead of disappearing as unknown
   protobuf fields.
2. **Engine and persistence — complete.** Added the import lifecycle using the
   provider-client interface. It validates the declaration and imported type,
   refreshes the partial result, validates the final state, and conditionally
   records exactly one absent address using the existing recovery machinery.
   Fake-provider cases cover a successful import with refresh and no apply,
   reference dependency/no-op behavior, unknown-output replacement handling,
   and sensitivity refusal. Failure injection for malformed import results,
   refresh errors, concurrent writes, and recovery remains additional coverage.
3. **Command and policy — complete.** Added the top-level import action and
   arguments, full configuration and provider-environment routing,
   `allowInfrastructure` policy, target-scoped identity guards, ownership and
   lock lifecycle, interruption handling, redacted output, and an action
   routing unit test. End-to-end CLI import coverage remains additional work.
4. **Documentation and coverage — complete.** Updated the infrastructure
   how-to, command and schema references, schema coverage matrix, this design
   specification's next-steps list, and `.agents/skills/cuenv-infrastructure/SKILL.md`.
5. **Independent adversarial review — complete for the draft.** Review found
   that unresolved references could repeatedly trigger replacement, sensitive
   outputs could lose their sensitivity through persisted state, import writes
   bypassed recovery, interruptions were not checked before every later RPC,
   and one failed consumer replan could stop unrelated operations. The
   implementation now resolves known outputs during planning, refuses
   sensitive references, writes import and final no-op updates through the
   recovery-aware recording path, checks interruption boundaries, and lets
   safe pre-mutation replan failures continue through the operation graph. The
   review also found that import success needed to show the next selected
   environment's plan command and occupied-address guidance needed to match
   commands that exist; both were corrected. Checks were added between
   provider initialization RPCs so an interrupt prevents subsequent validation
   and configuration calls.

## Verification plan

Use focused checks during implementation, then the full root flake check before
requesting review because this feature crosses provider protocol, engine,
policy, and state boundaries.

The focused verification checklist includes both protocol versions; unchanged
identifier and private-data forwarding; one, zero, multiple, and type-mismatched
provider results; warnings, errors, deferral, malformed state, and identity
metadata; refresh success and failure; an occupied target; absent-record
compare-and-swap; no-flag and named environment identities; policy withholding;
ownership and lock conflicts; lost state-write recovery; cancellation; and the
next ordinary plan after import. Run the infrastructure end-to-end and command
lifecycle suites with the fake provider and local state service. Run
`cuenv task ci.schema-docs-check` for the schema, documentation, and skill
updates. New fake-provider end-to-end cases cover import without apply,
dependency inference and no-op behavior for output references, unknown output
replacement handling, and refusing to flow sensitive outputs. The full
infrastructure end-to-end and CLI lifecycle suites still provide broader
coverage when their provider and local state-service prerequisites are present.

## Upstream references

- [Terraform resource instance change lifecycle](https://github.com/hashicorp/terraform/blob/main/docs/resource-instance-change-lifecycle.md)
- [Terraform Framework resource import](https://developer.hashicorp.com/terraform/plugin/framework/resources/import)
- [Protocol version 5 definition](https://raw.githubusercontent.com/hashicorp/terraform-plugin-go/main/tfprotov5/internal/tfplugin5/tfplugin5.proto)
- [Protocol version 6 definition](https://raw.githubusercontent.com/hashicorp/terraform-plugin-go/main/tfprotov6/internal/tfplugin6/tfplugin6.proto)
