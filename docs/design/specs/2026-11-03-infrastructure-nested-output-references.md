# Nested infrastructure output references

Status: implemented for native CUE selectors. The experiments below describe
the implementation snapshots on which they were run.

## Problem

Infrastructure references currently connect a top-level scalar provider output
to a top-level scalar configuration argument. A Cloudflare account token's
`value`, for example, cannot be supplied to the `config.token` argument of a
pipeline sink. Moving the consumer argument inside an object must not change
the dependency, type, planning, or sensitivity rules for the reference.

A native selector is an expression referring to an abstract output. Preserve
its CUE provenance and validate the actual provider output against the original
CUE constraints. Output references must not be encoded as provider configuration
objects or reconstructed from their field names.

## Intended contract

- Native selectors use CUE for the initial evaluation and for validating a
  consumer once the referenced output is known. Runtime outputs are unified
  into a frozen CUE evaluation in memory; they are not written to project files.
- Native CUE selectors are the only reference syntax. Remove the descriptor
  schema, generated type unions, manifest descriptor types, parser and
  compatibility adapter. Carry selector provenance as structured bindings
  separate from provider configuration; do not lower it into descriptor JSON.
- Source attributes remain declared top-level provider scalars. A value that
  carries one must have its type, and CUE computes a value derived from one, which
  must then suit its destination. Provider configuration, write-only and
  computed-only destinations remain unsupported. Output-dependent defaults, dynamic
  structure and source collection selection are refused. A native selector needs an
  abstract output declaration.
- Destination paths distinguish object fields, concrete map keys and ordered
  list or tuple indices. References beneath sets or dynamic-typed values are
  rejected. Ordinary provider objects must retain their field meanings, even
  when fields are named `fromResource`, `fromDataSource`, or `attribute`.
- An unknown producer stays unknown to the provider even if CUE constraints
  narrow the consumer expression to an apparently concrete value. Resolve and
  validate the actual output before the consumer's final provider plan. A
  replacement first revealed by that final plan must not mutate the consumer.
- A sensitive source marks the receiving top-level attribute as sensitive for
  persisted state and plan rendering. Register known sensitive strings before
  downstream calls, and do not expose unrestricted provider or CUE diagnostics
  when their contents cannot be redacted safely. Values can still be present in
  provider state and recovery files.
- The confirmed plan binds the selected CUE package, its imports, resource
  identities, dependencies, configuration shape and known values. Runtime
  specialization must not reread changed files or accumulate earlier fills.
  It must not permit an output to alter unconfirmed infrastructure changes.

## Descriptor removal and migration

The former `{fromResource, attribute}` and `{fromDataSource, attribute}` forms
are removed without a compatibility fallback. Replace them with direct selectors
such as `infrastructure.resources.primary.outputs.id` or
`infrastructure.data.lookup.outputs.value`. Untyped producers must declare the
selected output as a bare `string`, `number`, or `bool`; generated producer types
provide eligible declarations. Regenerate provider types with
`cuenv sync infrastructure` to remove the obsolete descriptor unions.

Provider object keys are not reserved globally. A literal object with fields
named `fromResource`, `fromDataSource`, or `attribute` remains ordinary provider
configuration when its provider schema accepts it. A descriptor-shaped object
at a scalar destination fails ordinary type validation and never creates a
reference or dependency edge. Test both cases, and keep all native selector
coverage for resource/data combinations, nested destinations, constraints,
sensitivity, ordering, replacement and deferred reads.

## Experiments so far

On the installed standalone CUE CLI (language version 0.17.1), an abstract
`string` output unified with a concrete overlay resolved its consumer selector
and an interpolation. A conflicting regular-expression constraint was rejected.
That error included the rejected synthetic value, demonstrating why CUE
diagnostics require a sensitive-value boundary.

CUE can also produce a concrete consumer while the producer remains abstract:

```cue
resources: {
    producer: outputs: value: string
    consumer: configuration: token: resources.producer.outputs.value & "expected"
}
```

Evaluating `resources.producer.outputs.value` gives `string`, while exporting
`resources.consumer.configuration` gives `{"token":"expected"}`. The engine
must retain the producer's unknown status independently of this CUE result.

An earlier experiment showed that unifying a descriptor object directly with
its resolved string fails because the values have different CUE types. The
final design removes that syntax instead of retaining a compatibility adapter.

The standalone CLI experiments did not exercise cuenv's pinned CUE fork. A
separate Go experiment run under `nix develop` imported the fork from
`crates/cuengine/go.mod`. It captured an instance and an imported closed schema,
removed the source files, rebuilt fresh contexts and filled string, number and
boolean values. It resolved a `let` alias and nested list destination and
rejected incompatible literal and length constraints. That experiment was a
proof of the approach, not a Go/Rust bridge or infrastructure lifecycle test.
The proof and integration gates below cover the remaining requirements.

A fake provider with a single nested consumer attribute also reproduced
the previous limitation: an ordinary literal configuration passed, while six
nested descriptor cases failed with `config.token: expected string, got an
object`. No live Cloudflare service was used.

An additional experiment showed that CUE can hide an output reference inside a
conditional, comprehension, dynamic label or index when a constraint narrows
the source to a concrete value. These structural uses need explicit fail-closed
checks; inspecting only the evaluated leaves is insufficient.

The official Cloudflare 5.28.1 provider was tested with a loopback-only fake
API and synthetic credentials. An in-progress engine successfully passed the
token to the sink, preserved the create-only token when later reads omitted
it, and replaced the sink when the token changed. A separate synthetic error
revealed a missing provider-credential redaction registration; with the local
fix, the provider error test masks both the copied token and the provider
credential. These experiments do not establish behavior against a live
Cloudflare account.

Terraform's Go codecs decode numbers with finite precision. Experiments found
that very large distinct integers can collapse to the same value and that a
large integral float can change on a provider decode/encode round trip. Numeric
values therefore use an infrastructure-local exact decimal representation;
unsafe provider-bound values fail before the call instead of rounding silently.
Values received from a provider remain recordable even when they cannot later
be sent safely. State uses a typed numeric string where ordinary JSON cannot
preserve the value, and cannot repair state rounded by older cuenv versions.
The interoperability fixture exercises the protocol 5 and 6 codecs and typed
JSON state with vendored dependencies in the Linux infrastructure E2E check.

The opt-in `BenchmarkFrozenImportedGraph` exercises one 64-entry consumer with
192 reference leaves and a 35-instance import graph (9,255 source bytes). One
local run of 24 specializations after global dependency checks were added
measured 218.5 ms and 300.1 MB allocated per call, with no increase in retained
heap after garbage collection. The benchmark does not supply the complete
reference and source scope used in production, and is not a multi-consumer plan
benchmark or a peak-RSS measurement. A plan can specialize each native consumer
more than once. Rebuilding and checking the graph per consumer can be expensive;
measure full planning at larger scales before
claiming that this approach scales to large stacks. Initial evaluation and each
runtime specialization have bounded waiting and bounded worker admission, but
an abandoned Go evaluation can retain memory and values until it finishes or
the process exits; these limits do not forcibly cancel CUE.

A separate bridge-only experiment used the full reference scope for consumers
with three references each and a 35-instance import graph. It executed three
sequential specializations per consumer to model unknown preflight, unknown
planning and known apply. Median results over three runs were:

| Consumers | Seconds | Cumulative allocation |
| ---: | ---: | ---: |
| 1 | 0.039 | 0.046 GB |
| 8 | 0.859 | 1.188 GB |
| 32 | 11.542 | 16.011 GB |
| 64 | 46.337 | 62.085 GB |

A second in-progress source snapshot reproduced 45.8 seconds and 62.1 GB at
64 consumers. A prepared capability now reuses only the original snapshot's
verified reference and dependency analysis. Every specialization still builds
a fresh CUE context, fills its own values and validates the resulting instance.
Three paired runs of the same workload, including initial freeze, inspection,
validation and preparation, measured these medians:

| Consumers | Fully checked | Prepared | Fully checked allocation | Prepared allocation |
| ---: | ---: | ---: | ---: | ---: |
| 32 | 11.726 s | 1.121 s | 16.356 GB | 0.805 GB |
| 64 | 45.546 s | 2.906 s | 62.759 GB | 1.875 GB |

The CLI's additional bootstrap inspection was not included in either variant.
These figures are allocation churn, not peak RSS or end-to-end CLI timing, and
do not prove linear scaling. Run the opt-in experiment with:

```sh
CUENV_FROZEN_SCALING=1 GOMAXPROCS=4 SCALING_LAYERS=32 SCALING_REPETITIONS=3 \
  go -C crates/cuengine test -timeout 30m -run '^TestScalingReviewProductionScope$' -count=1 -v
```

Set `CUENV_FROZEN_SCALING_VARIANT=prepared` to measure the prepared path with
the same fixture. `setup_ms` and `setup_allocated_bytes` include preparation;
add them to the three phase measurements when comparing totals.

## Output provenance after issue 566

Release 0.65.0 failed in both directions. Valid CUE unrelated to any output was
refused, because reparsing an expression lost the package identity of its hidden
names. An output-dependent conditional in an imported definition was accepted with
no dependency, because `ReferencePath` reports a shared definition by its declared
path and a reference rooted outside the instance counted as no dependency. On the
second failure a fake provider applied the consumer before its producer.

Both came from rebuilding from source text what the evaluator already knows, and every
review of that approach found another gap. The evaluator resolves each reference through
the environment it was bound in. It only had no public API to report that, so the pinned
CUE fork now has one, tracked upstream as cue-lang/cue#4547.

1. **Provenance from the evaluator.** `provenance.Analyze` in the fork's `cue/provenance`
   walks the conjuncts of the fields it is asked about, and of whatever those read, through
   those environments. It evaluates the instance itself, with structure sharing off so a
   definition's fields exist where it was instantiated, and evaluates nothing else, so the
   rest of a project costs nothing. It reports each reference
   as a field, the field it resolves to, and how it is used: `Value` (unified in, possibly
   constrained), `Computed` (an operand), `Control` (a condition, label, index or the arm
   of a disjunction) or `Presence` (which fields exist). A let, a comprehension variable and
   a field of an inline value have no path of their own, so what they read is reported for
   the field using them, along with what decides they are there: in
   `[if c {"a"}, "b"][0]`, `c` selects the element. Conditions are reported even when
   evaluation consumed them and their results left no trace. A field pushed down from a
   comprehension is reported even when its arc is gone because the condition did not
   resolve. A field with more than one arm of a disjunction still alive reports
   `Alternatives`, because what its fields turn out to hold can change the arm. Anything it
   cannot walk is an error, and refuses the instance rather than being treated as
   independent.
2. **A graph from the configuration to the outputs.** `referenceGraph` in the bridge follows
   these from each exported value under the configuration roots. A value depends on what
   it refers to, on what decides that it exists, and when read whole on everything below
   it. A struct unified into a field is followed field by field, so a copy of a resource
   maps to its output. Reading which fields a struct has reads no values, but the presence
   of a scalar depends on its value, because CUE does not consider an abstract value present.
3. **Verification with real values.** `cue_verify_frozen` unifies the outputs that actually
   exist and compares the exported configuration. It runs at the end of planning, after each
   completed change of a resource declaring outputs, after a deferred read, and when apply
   finishes, only using outputs whose resource has no remaining work. A declared scalar whose
   real value is a list or object cannot reach CUE, so it is refused rather than ignored.
   Failing while planning changes nothing. Failing during apply skips everything not yet
   started, including deferred reads, and reports an incomplete apply, exit code 5. It cannot
   undo a change that already ran.
4. **Exported values stay ordinary CUE.** Exports, fills and final validation run on a
   normal build. Tests compare both modes on every accepted fixture and several values.

The sampling step and the source analysis were removed. Compared with the previous release
the bridge is about 1,250 lines smaller, and the fork adds about 1,150. Sampling could only refuse and tried values that real
outputs may never have, so it refused what depended on nothing real.

### Admitted and refused forms

Admitted: a scalar configuration value in `resources` or `data`, including inside nested
objects, maps and ordered collections, however its reference is written and however many
it reads. That covers aliases, lets, hidden fields, imported definitions, `(#F & {in: x}).v`,
`({in: x, v: in}).v`, `self`, `try y = expr`, `W={...}` read as `W.z`, loop variables, and
bodies of conditions and loops that are themselves known. A value that reads one output
through constraints carries it. Anything else is computed: interpolation, arithmetic,
builtins, or two outputs unified together. It stays unknown until every output it reads is
known, and CUE computes it from the real values.

Refused before any provider change: conditions, labels, patterns, indexes, slices and
defaults that depend on an output, a presence guard on a field holding one, and a list or
struct computed from one, wherever they are reached from. These decide what fields exist,
so a plan could not say what would be applied. Terraform refuses the same for `count` and
`for_each`.

Not refused: a check the configuration does not read, such as
`_check: infrastructure.resources.a.outputs.id & =~"^x"` or a hidden comprehension. It
cannot change what is sent to a provider, and real values are verified against it, so a
violated check stops the apply as soon as that value exists.

### Evidence

`cue/provenance/provenance_test.go` in the fork asserts reported dependencies, including
definitions binding to their instance, shadowing, hidden names of the same spelling in
different packages, and let and comprehension variables through inline values.

`frozen_provenance_matrix_test.go` asserts exact edges. `frozen_provenance_generated_test.go`
crosses 11 controls with 21 wrappers and 5 placements, each with an unrelated twin.
`frozen_provenance_attack_test.go` covers further syntax and ways of reaching a stack, and
requires no valid form to be refused unless it decides structure. A fill oracle, which can
show a dependency and never clear one, checks every accepted fixture.
`TestFrozenVerificationFollowsRealValues` checks that verification refuses exactly the
accepted fixtures that values change, and every value CUE refuses.
`TestFrozenChecksRefuseAWrongReferenceTarget` gives verification a recorded edge to the wrong
source. `TestFrozenComputedReferences` covers partly known values and violated constraints on
computed ones.

An independent adversarial review of this analysis generated about 8,000 programs and found
six classes of missed dependence, all of which had passed the tests above: an arm selected
by a field holding an output, a selected list element with a condition beside it, a let's error
changing a comparison with bottom, an arm of a default in a `for` source, an explicitly opened
reference, and hidden fields of the same name in different packages collapsing into one. Those
are now tests. Verification had caught each of them with a real value, except where the value
matched what the analysis assumed, which changes nothing. It also found a crash in the fork's
own sharing patch, on `x: 1, #D: {if true {a: x}}, s: #D`, which is present in earlier releases.

This is finite evidence, not a proof. The analysis now reads what the evaluator bound, so
what it misses is limited to expressions the walker classifies wrongly rather than scoping it
reimplements, but that review shows such cases existed after every earlier test passed. A form
it refuses to walk fails closed. One it walks and misclassifies is caught when its real value
is known, which can be after an unrelated resource has changed.

The schema's checks moved into `_unresolved: {...}`. While they were comprehensions on the
configuration itself, reading any field of it forced them, and they read every resource. A
resource reading an output through a field or let outside the configuration then read itself,
which the pinned evaluator and CUE 0.17.1 report as a structural cycle (0.18 does not). Error
paths are unchanged.

The pinned evaluator evaluates `o: "zzz"; let M = o & "x"; y: M == "x"` to `true`, so a
conflict inside a let can vanish and final validation does not see it
(cue-lang/cue#4548). A structural use of this is refused, but a computed one such as
`"\(M)"` evaluates to `"x"` instead of failing.

`crates/cuenv/tests/infrastructure_lifecycle.rs` drives the real schema, command line,
bridge and fake provider over ordering, stored edges, unknown values, computed values,
constraint violations, sensitive nested and computed destinations, final-plan replacement,
chains and separate packages.

### Snapshot consistency

The loader reads each file twice, imports first, one file at a time. A test edits two
files between those reads and reproduces a snapshot pairing old bytes of one file with
new bytes of another, a revision that never existed. Freezing now requires two
consecutive loads with identical bytes, and refuses after four attempts. Package
filtering is recomputed per load, so a file joining the package is seen too. This doubles
loading work. An owning project and a separate infrastructure package are loaded by
separate operations, so a project edited between them is not covered. Editing a file
during the confirmation prompt was exercised against the built binary with a terminal
and applied the confirmed value.

### End-to-end cost

One local run per size, with the real schema, the fake provider and a loopback `sqld`,
one producer and N consumers that all declare an output. Peak is the command's maximum
resident set size. Time is wall clock.

| Consumers | 0.65 plan | Plan | Plan peak | 0.65 apply | Apply | 0.65 apply peak | Apply peak |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | 0.20 s | 0.17 s | 70 MB | 0.33 s | 0.31 s | 92 MB | 71 MB |
| 8 | 0.31 s | 0.25 s | 75 MB | 0.50 s | 0.51 s | 97 MB | 87 MB |
| 32 | 0.64 s | 0.58 s | 78 MB | 1.34 s | 1.60 s | 97 MB | 83 MB |
| 64 | 1.40 s | 1.23 s | 95 MB | 2.90 s | 3.33 s | 105 MB | 96 MB |
| 128 | 3.38 s | 3.32 s | 93 MB | not run | not run | not run | not run |

An earlier version filled one output at a time, which took 10 seconds and 538 MB to apply
64 consumers and a measured 223 seconds for one verification of 500 resources with 5 outputs
each. Unifying all values at once took that verification to 96 milliseconds. Verification
remains a full CUE build per completed change of a resource declaring outputs, so apply stays
above linear in such stacks. Larger stacks have not been measured through the CLI.

### Cancellation

Deadlines apply per operation, with `CUENV_EVAL_TIMEOUT` defaulting to 60 seconds. At
`CUENV_EVAL_TIMEOUT=1`, the 128-consumer plan finished in 3.37 seconds because no single
operation reached the deadline; an earlier, slower build stopped with exit code 3 after 1.42
seconds, which is the behaviour when one does. CUE cannot be cancelled: the caller stops waiting, and an unfinished worker keeps its memory,
including any filled values, and one of 16 process-wide slots until it returns or the
process exits. A verification that times out or finds no free slot stops the run rather
than continuing unverified.

### Limits

A value computed from an output can have any shape. `strings.Split(x, ",")` is refused while
planning at a destination whose provider type is a list, because a reference needs a scalar
destination. At a scalar destination it stops the apply with a type error once its value is
known. CUE decides the shape of such a value, so analysis cannot say more ahead of time.

Analysis costs what the configuration reaches, not what the project holds. On cuenv's own project,
one field takes 1.4 ms, its generated workflows 1 second, and all of it 1.1 seconds, where the first
version of this analysis ran out of memory at 37 GB. That had three causes: finishing the instance
first, which instantiated every definition with sharing off whether used or not; arms of a disjunction
that still claimed to be under evaluation, so each reference read through one evaluated all of it
again; and evaluating every inline struct that was an operand afresh, when its references can simply be
walked. `TestFrozenAnalysisOfALargeProject` freezes that project, as nothing smaller showed these costs.

Definitions that fan out are still exponential with sharing off if the configuration does reach them:
`#T1: {a: #T0, b: #T0}` doubles at each level. Exporting such a value would be as well. The evaluation
timeout bounds the wait but not the memory, because CUE cannot be cancelled. Generated provider types
form trees, which are linear, but no provider as large as Cloudflare has been measured through this path.

A comprehension that adds a field with a dynamic label, beside a field the configuration reads,
is refused if an output decides it, even when the labels differ: `(expr)` could produce any label.

An output declared with the wrong type for its provider, such as `number` for a string attribute,
is refused while planning, whether or not anything reads it. A declared output the provider never
returns, or returns as null, is not checked; a reference to one still fails when it is resolved.

### `sqld` write contention

`sqld` 0.24.32 can stop serving writes for good under write contention, while still answering reads
and `/health`. With thirty-two tests sharing one server, it did so in three of four full flake runs.
The defect is in `libsql-server/src/connection/connection_manager.rs`, and upstream's main branch
still has it:

1. A writer that has to queue pushes itself onto `write_queue` and parks.
2. When it wakes and another connection holds the slot for a checkpoint, `acquire` returns
   `SQLITE_BUSY` and its entry stays in the queue, as the comment beside that check warns.
3. The connection closes. `close` releases the slot only if the connection holds it then.
4. A later `schedule_next` takes the dead entry and gives the slot to that connection as `Notified`.
5. Only the owner of a slot moves it on from `Notified`, and other writers only wait, spinning once
   the slot's deadline passes. Nothing times out a `Notified` slot.

The captured log shows this exactly: connection 4006 closed at 21:24:43.787629, and was given the
slot at 21:24:43.813194, which it held until the server was killed. A client cannot prevent this.
It needs concurrent writers and a checkpoint, which `sqld` runs every 1,000 pages of log.

What this means for a run: while the server is stuck, a state write fails after four attempts of
thirty seconds, and the run stops as it does for any state store failure. Releasing its lock then
takes as long. A loopback proxy that stops answering writes, as a stuck server does, ended a run this
way after 273 seconds, naming the timeout. A write that follows a provider change stops with an
unrecorded change and a recovery file. Restarting `sqld` clears it. Whether Turso Cloud shares this
code is unknown.

Two things changed because of it. Tests no longer share a server, as a shared database made every
test contend with every other for the sake of nothing they test, and one stuck writer failed them
all. And the store no longer gives up on `SQLITE_BUSY` after four attempts, about 1.4 seconds of
backoff, which failed five of 27 loaded runs at four tests at a time. `SQLITE_BUSY` means the
database did nothing and another writer will finish in moments, so it is retried for thirty seconds,
with jitter so that colliding writers do not collide again.

## Proof and integration gates

1. Prove in-memory specialization against the pinned CUE fork and bridge,
   including closed imported definitions, aliases, partial fills, fixed nested
   paths, constraints, exact scalar values, snapshot isolation and safe errors.
   Exercise unknown producer values narrowed by CUE and measure deep-graph
   memory and repeated-specialization cost.
2. Connect structured bindings to the resource/data graph, provider schema
   cursor, planning, final replanning, state dependency persistence, sensitivity
   and recovery. Preserve invocation-local data reads and destroy ordering.
3. Exercise native selectors with a fake provider, including sensitive unknown values,
   known/no-op cases, type mismatches, dependency cycles, repeated references,
   and a replacement first revealed by the final plan.
4. Test the pinned Cloudflare provider against a loopback-only fake API with
   synthetic credentials and tokens. A live Cloudflare deployment is separate
   evidence and is not required by this design.
5. Update the infrastructure how-to, schema coverage matrix and infrastructure
   skill. Run focused CUE, Rust, fake-provider and schema checks, then the full
   root flake check before requesting review or marking the PR ready.
