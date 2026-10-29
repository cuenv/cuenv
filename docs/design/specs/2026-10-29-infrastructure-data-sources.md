# Provider data sources

Issue: [#545](https://github.com/cuenv/cuenv/issues/545). Included with sensitive
output references in [#544](https://github.com/cuenv/cuenv/pull/544).

Infrastructure remains experimental. This increment adds read-only provider
lookups with the existing scalar reference contract.

## Configuration and identity

`infrastructure.data` contains `#DataSource` declarations: `type`, optional
`provider`, `configuration`, scalar `outputs` and `dependsOn`. A named
environment replaces the entire providers/resources/data configuration. Data
and resource names occupy distinct namespaces. Bare `dependsOn` names still
refer to managed resources; output references infer edges to either kind.

When named environments exist, a data-only top level is allowed only with no
managed state in the no-flag identity. Adding a lookup must not disable the
legacy-state guard. Without named environments, ordinary orphan deletion
still applies to previously managed resources removed from configuration.

`#InfrastructureOutputReference` accepts exactly one of `fromResource` and
`fromDataSource`, together with `attribute`. Both addresses use `type.name`.
Native selectors through either namespace's `outputs` lower to that same
representation. Only whole top-level scalar arguments and outputs are
referenceable. Collection configuration and provider results are otherwise
supported.

Generated packages are `data/<type>/data_source.cue` with `#DataSource`,
`#Configuration` and scalar `#Outputs`. Schema digests include nonempty data
source schemas. Existing pins for providers exposing data sources must be
refreshed with `provider add <source>@<same-version>`; sync never silently
repins. Providers without data sources retain their previous digest.

## Protocol and execution

Both protocol versions expose data source schemas at response field 3.
Validation is `ValidateDataSourceConfig` in protocol 5 and
`ValidateDataResourceConfig` in protocol 6. Both read with `ReadDataSource`.
cuenv does not advertise provider-requested deferral support; an unsolicited
protocol deferral is an error.

A mixed dependency graph distinguishes managed resources from data sources.
Known, stable lookups are read during planning. A read waits until apply when
its configuration contains an unknown reference or a prerequisite managed
resource has a pending change. Known IDs do not make a read safe before an
update: the remote object behind the ID can change. Deferred data prerequisites
also defer their consumers. Cycles are rejected before execution.

Every CLI plan/apply invocation creates its own fresh plan. Completed reads
remain the inputs of that plan through confirmation. Deferred reads run after
their prerequisites finish, and their results resolve consuming resources'
final plans. A newly discovered replacement is still refused until a fresh
plan can show it. Planning failures abort the plan; deferred-read failures
during apply skip dependent work, retaining unrelated work.

## State and sensitivity

Data results belong to the invocation, never a `ManagedResource` row, an
upgrade-state RPC or an unrecorded-change recovery file. Destroy neither
reads nor deletes data sources. Managed resources retain their transitive
managed prerequisites through data nodes so that later removal of a data
declaration cannot erase destroy ordering.

Values copied from lookups into managed arguments can still occur in managed
state and recovery files. Managed records persist a canonical
`sensitive_attributes` list in SQL schema version 2. A transactional, lock-fenced
migration from version 1 initializes existing rows to `[]` without changing
their provider state/private data. It also renames the SQL `state_json` column
to `resource_state_json`: a paused v1 reader that already checked the old
schema version cannot subsequently read new values without sensitivity marks
using its old projection. Historical copied-value provenance cannot
be reconstructed from those empty marks. Incompatible pre-migration-table
development layouts remain refused.

Sensitivity marks are unioned across retained-object writes, including
refreshes after removing references and recovery overwrites. Recovery's
separate `FileResource` representation carries the defaulted field; absent
legacy metadata defaults to empty, malformed present metadata fails. Older
readers reject the unfamiliar field rather than dropping it. Schema upgrades
that could rename sensitive attributes conservatively mask the returned
top-level attributes; old names alone cannot protect renamed values. Unmapped
historical sensitive names also trigger conservative masking when the provider
has not changed its schema version.

Provider-declared sensitivity and sensitivity
inherited by reference destinations must survive reference chains. Register
known sensitive strings before validation/read calls, and returned sensitive
strings before processing response diagnostics. Numeric/boolean values and
strings shorter than four characters are masked in plans but not covered by
the string redaction registry. Derived copies are not tracked.

Sensitive map keys are registered alongside values, including stored JSON and
best-effort extraction from malformed responses. If an update returns
undecodable state, preserve effective sensitivity on the retained prior record
without accepting the malformed state; a failed metadata write uses the normal
recovery path. A later fresh-process refresh must not lose newly inherited
marks merely because the preceding mutation returned invalid state.

## Architecture review requirements

The initial read-only review identified these blocking cases, addressed by
the execution contract above and requiring regression evidence:

- Known IDs with changing prerequisites must defer reads.
- Data nodes must never enter managed persistence or delete paths.
- Managed ancestors must survive data-source indirection in stored dependencies.
- Colliding names require namespace-aware graph keys.
- Sensitive configuration and returned values must be registered before
  provider diagnostics can expose them.

The schema/CUE milestone review found no blocking defects. Its digest-migration
finding was confirmed: sync incorrectly attributed an expected digest update
to a different provider binary. The diagnostic now describes both schema and
cuenv digest changes while retaining explicit same-version repinning.
The protocol/security boundary review verified both wire protocols and found
no blocking defects. Its redaction-precondition documentation request was
implemented on the two client methods; computed-block support remains a
documented follow-up below.

The platform-operator review additionally requires transitive readiness through
unchanged managed intermediates, one terminal outcome per deferred read even
without consumers, reuse of completed reads during final replans, and interrupt
checks after the last read and before reporting no-work success. It also found
that adding a top-level lookup could bypass the legacy-state guard; the guard
now permits data-only configurations beside named environments only when the
no-flag identity has no recorded managed resources.

The final runtime review confirmed further acceptance requirements:

- Undecodable data results must fail without rendering unrestricted provider
  diagnostics, which can repeat sensitive attributes that failed registration.
- Final managed replans must register destination-sensitive configuration even
  when its source data attribute is not provider-marked sensitive.
- Lookup-derived sensitivity must survive fresh-process destroy/orphan plans,
  reference removal, schema upgrades and recovery; this requires the persisted
  metadata above rather than only an invocation-local redaction registry.
- A successful deferred read after cancellation must not permit the next
  managed operation to start. Read failures after cancellation must report an
  interrupt, without claiming that a managed mutation has an unknown outcome.
- A failed data read is the failing operation; its unexecuted consumers must
  be reported as skipped, preserving dependent failure propagation and
  replacement aftermath.

The state-format review found that a purely additive migration left paused v1
readers able to read v2 state without metadata. Invalidating their old SQL
projection in the same migration transaction closes that race. Recovery uses
one initial sensitivity snapshot rather than rereading all state per file;
its existing retry-acknowledgment checks can still reread after a conflict.
An actual metadata-absent recovery-file regression verifies forced recovery
retains sensitivity, generation and serial advancement, then removes the file.

The first full Nix gate exposed concurrent migration announcements racing on a
fresh Turso database. Announcements now use an atomic, version-conditional
upsert, completed migrations remove their announcements in the same transaction,
and a caller whose migration already completed does not wait on current-schema
locks. Withdrawal is scoped to its migration version. Deterministic paused-reader
and concurrent-announcement regressions cover D1 emulation and real Turso; the
61 fake-provider cases also pass against a fresh shared Turso server.

The final CLI review found that the fresh-process regression used JSON plans,
which omit attribute values and therefore did not prove masking. The regression
now checks text orphan-plan and destroy previews for sensitive placeholders and
forces a refresh diagnostic to echo the stored secret after declarations have
been removed.
The focused test passes with sensitivity intact. A temporary mutation that
cleared the stored marks made the text removal-plan assertion fail and exposed
the fixture's secret, confirming that the regression detects lost provenance;
the mutation was then removed.

The final security review additionally identified sensitive-map-key traversal
and retained metadata after malformed apply responses. The CLI regressions
exercise a diagnostic echoing a sensitive map key and a remote update that
commits a lookup secret but returns malformed state, followed by fresh-process
orphan/destroy previews and refresh diagnostics.

Validation evidence and final runtime adversarial findings are recorded on the PR
after integration. The root flake gate is required because this changes
cross-crate runtime behavior, provider boundaries and CLI/schema support.

## Next steps

- Migration announcements are shared by version. A timing-out same-version
  waiter can withdraw another waiter's announcement, weakening starvation
  prevention while the transactional lock fence remains intact. Track waiter
  ownership or leases before strengthening migration liveness guarantees.
- Structured nested/collection selection, including the Cloudflare permission
  group example from #545. Lists need explicit indices, maps explicit keys;
  unordered sets cannot safely support positional selection. No implicit
  first-match selection or expression evaluation is introduced here.
- Provider functions, ephemeral resources and protocol-requested deferrals
  remain unsupported.
- Protocol 6 computed blocks are not advertised as supported. The existing
  schema subset omits their flag; providers must reject a configuration that
  needs this capability. This is unchanged by data source support.
