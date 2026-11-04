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
- Source attributes remain declared top-level provider scalars. Source and
  destination scalar types must match; provider configuration, write-only and
  computed-only destinations remain unsupported. Arithmetic, interpolation,
  output-dependent defaults, dynamic structure and source collection selection
  are outside this change. A native selector needs an abstract output
  declaration.
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

The fix keeps the pinned CUE fork unchanged and uses four layers. None of them alone
is a proof, and no layer treats an expression it does not model as safe.

1. **Analysis with bindings kept.** Analysis builds with structure sharing off, so
   references report the path where a definition was instantiated. Expressions
   resolve with the package identity of their source file. Unknown syntax is treated
   as a possible control, only the values of data fields are excluded, and a
   non-concrete value under a root that is neither this instance, an imported package
   nor a let is an error. `sourceTargets` follows references toward every declared
   source and stops each branch at the first one, because fills flow from a source to
   its aliases and never back. A reference with more than one source is not direct.
2. **Sampling before anything changes.** Analysis can miss a read whose result is
   concrete while the source is abstract, as with a default over a narrowed source.
   `rejectSampledDependence` fills every source with a value chosen to differ from
   everything and compares the exported configuration. Any change outside the output
   namespaces and exact reference leaves refuses the instance, and a reference leaf must
   carry exactly its value. A leaf whose constraint refuses the sample excludes that
   source and the check repeats for the rest. This can refuse and never clear: it cannot
   prove independence from a value it did not try.
3. **Verification with real values.** `cue_verify_frozen` applies the same comparison to
   the outputs that actually exist, with all of them unified at once. It runs at the end
   of planning, after each completed change of a resource declaring outputs, after a
   deferred read, and when apply finishes. It only uses outputs whose resource has no
   remaining work, because a recorded output about to be replaced says nothing about
   what will be applied. A declared scalar whose real value is a list or object cannot
   reach CUE, so it is refused rather than ignored. Failing while planning changes
   nothing. Failing during apply skips everything not yet started, including deferred
   reads, and reports an incomplete apply, exit code 5. It cannot undo a change that
   already ran, so it does not replace the layers above.
4. **Exported values stay ordinary CUE.** Exports, fills and final validation run on a
   normal build. Tests compare both modes on every accepted fixture and several values.

### Admitted and refused forms

Admitted: a direct reference through aliases, lets, hidden fields, imported
definitions, field aliases and reference-free constraints, at a scalar destination
including nested objects, maps and ordered collections. Comprehensions, dynamic
labels and indexes may use anything that does not depend on an output. `for` over
containers holding outputs may read their keys and unrelated fields.

Refused before any provider change: conditions, labels, patterns, indexes, slices,
arithmetic, interpolation and builtins depending on an output, including through
`close`, `and`, `or`, defaults, selections and lets.

These valid forms cannot be checked and are refused even when unrelated to any
output: reading a data field from a definition instantiated inline, such as
`(#F & {in: x}).v`, the same selection embedded in a struct, a literal read the same way
such as `({in: x, v: in}).v`, `self`, `try y = expr`, and value aliases of a whole struct
such as `W={...}` read as `W.z`. The same definition reached through a field,
`_t: #F & {in: x}` then `_t.v`, and a control inside `(#F & {in: x}).out` are admitted.
Refused as well, as in earlier releases: a reference inside a conditional or loop body
such as `if _e {name: x}`, a `let` clause interpolating a loop variable, and a presence
guard on a field holding a reference, whose result differs between plan and apply. A resource copied
whole into another field, such as `_c: infrastructure.resources.producer`, is refused
by CUE itself under the closed schema. `{a: 1}[x]` inside a `for` panics in the pinned
evaluator's `Expr`, which is recovered and refused.

### Evidence

`frozen_provenance_matrix_test.go` asserts exact edges and that a rejection came from
a dependency rule rather than a failed inspection, unless explicitly marked as a refusal.
`frozen_provenance_generated_test.go` crosses 11 controls with 21 wrappers and 5
placements, each with an unrelated twin. `frozen_provenance_attack_test.go` covers
further syntax and ways of reaching a stack, and pins the exact lists of refused forms,
so a new refusal or panic fails the test. A fill oracle, which can show a dependency and
never clear one, checks every accepted fixture. `TestFrozenVerificationFollowsRealValues`
checks that verification refuses exactly the accepted fixtures that values change.
`TestFrozenChecksRefuseAWrongReferenceTarget` gives sampling and verification a recorded
edge to the wrong source; removing the equality check they share fails it.

This is finite evidence, not a completeness proof. Review repeatedly found new classes
of escape in analysis alone: default markers, `close`, container copies, defaults over
narrowed sources, and hidden names colliding across packages. One fix that resolved a
binding turned a safe refusal into an escape, so a refused form must only become admitted
with both of its twins tested. A second review of the finished layers found no case
that verification missed; the cases that passed analysis and sampling all needed a real value
the samples did not try, such as `(x & =~"^x") != _|_` inside a builtin. Those specific forms
are now analysed, but others like them can exist: when one does, the run stops as soon as the
real value is known, which can be after an unrelated or mis-ordered resource has changed.

The pinned evaluator evaluates `o: "zzz"; let M = o & "x"; y: M == "x"` to `true`, so a
conflict inside a let can vanish and final validation does not see it. Analysis
rejects the output-dependent forms of this, and verification compares results. Whether
current upstream CUE behaves the same has not been checked.

`crates/cuenv/tests/infrastructure_lifecycle.rs` drives the real schema, command line,
bridge and fake provider over ordering, stored edges, unknown values, constraint
violations, sensitive nested destinations, final-plan replacement, chains and separate
packages.

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
| 1 | 0.20 s | 0.36 s | 105 MB | 0.33 s | 0.44 s | 92 MB | 108 MB |
| 8 | 0.31 s | 0.49 s | 132 MB | 0.50 s | 0.77 s | 97 MB | 136 MB |
| 32 | 0.64 s | 0.97 s | 115 MB | 1.34 s | 1.84 s | 97 MB | 117 MB |
| 64 | 1.40 s | 1.96 s | 156 MB | 2.90 s | 4.24 s | 105 MB | 126 MB |
| 128 | 3.38 s | 4.66 s | 144 MB | not run | not run | not run | not run |

An earlier version filled one output at a time, which took 10 seconds and 538 MB to apply
64 consumers and a measured 223 seconds for one verification of 500 resources with 5 outputs
each. Unifying all values at once took that verification to 96 milliseconds. Verification
remains a full CUE build per completed change of a resource declaring outputs, so apply stays
above linear in such stacks. Larger stacks have not been measured through the CLI.

### Cancellation

Deadlines apply per operation, with `CUENV_EVAL_TIMEOUT` defaulting to 60 seconds. At
`CUENV_EVAL_TIMEOUT=1`, the 128-consumer plan finished in 4.65 seconds because no single
operation reached the deadline; an earlier, slower build stopped with exit code 3 after 1.42
seconds, which is the behaviour when one does. CUE cannot be cancelled: the caller stops waiting, and an unfinished worker keeps its memory,
including any filled values, and one of 16 process-wide slots until it returns or the
process exits. A verification that times out or finds no free slot stops the run rather
than continuing unverified.

### Limits

An output declared with the wrong type in an untyped configuration, such as `number` for a
string attribute or `string` for a list, is not caught while planning: the resource is
created and the run then stops naming that output, and plans stay refused until the
declaration is corrected or the resource destroyed. Generated provider types declare the
correct type. A declared output the provider never returns, or returns as null, is not
checked; a reference to one still fails when it is resolved.

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
