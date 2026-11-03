# Nested infrastructure output references

Status: implementation and experiments in progress. The behavior described as
a goal below is not yet fully validated.

## Problem

Infrastructure references currently connect a top-level scalar provider output
to a top-level scalar configuration argument. A Cloudflare account token's
`value`, for example, cannot be supplied to the `config.token` argument of a
pipeline sink. Moving the consumer argument inside an object must not change
the dependency, type, planning, or sensitivity rules for the reference.

The native selector and descriptor forms (`{fromResource, attribute}` or
`{fromDataSource, attribute}`) have different CUE semantics. CUE evaluates a
descriptor as an object. A native selector is an expression referring to an
abstract output. The descriptor must remain supported, but converting a native
selector to a descriptor and filling it only after CUE export cannot validate
the actual provider output against the original CUE constraints.

## Intended contract

- Native selectors use CUE for the initial evaluation and for validating a
  consumer once the referenced output is known. Runtime outputs are unified
  into a frozen CUE evaluation in memory; they are not written to project files.
- A descriptor remains an explicit compatibility binding. CUE evaluates the
  descriptor object, then the engine resolves the scalar in the projected
  provider configuration. Both forms share provider-schema validation,
  dependency ordering, unknown handling, sensitivity and final-plan checks.
- Source attributes remain declared top-level provider scalars. Source and
  destination scalar types must match; provider configuration, write-only and
  computed-only destinations remain unsupported. Arithmetic, interpolation,
  output-dependent defaults, dynamic structure and source collection selection
  are outside this change. A native selector needs an abstract output
  declaration; an existing descriptor does not.
- Destination paths distinguish object fields, concrete map keys and ordered
  list or tuple indices. References beneath sets or dynamic-typed values are
  rejected. Schema-directed recognition must preserve existing provider
  objects that happen to contain descriptor field names.
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

Finally, unifying a descriptor object directly with its resolved string fails
because the values have different CUE types. This is why descriptors need a
compatibility adapter rather than runtime unification at the descriptor field.

The standalone CLI experiments did not exercise cuenv's pinned CUE fork. A
separate Go experiment run under `nix develop` imported the fork from
`crates/cuengine/go.mod`. It captured an instance and an imported closed schema,
removed the source files, rebuilt fresh contexts and filled string, number and
boolean values. It resolved a `let` alias and nested list destination and
rejected incompatible literal and length constraints. This is a proof of the
approach, not yet a Go/Rust bridge or infrastructure lifecycle test; the
permanent bridge tests must also cover shared imports, numeric precision,
snapshot ownership, safe diagnostics and repeated-specialization cost.

A fake provider with a single nested consumer attribute has also reproduced
the current limitation: an ordinary literal configuration passed, while six
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

## Proof and integration gates

1. Prove in-memory specialization against the pinned CUE fork and bridge,
   including closed imported definitions, aliases, partial fills, fixed nested
   paths, constraints, exact scalar values, snapshot isolation and safe errors.
   Exercise unknown producer values narrowed by CUE and measure deep-graph
   memory and repeated-specialization cost.
2. Connect structured bindings to the resource/data graph, provider schema
   cursor, planning, final replanning, state dependency persistence, sensitivity
   and recovery. Preserve invocation-local data reads and destroy ordering.
3. Exercise both forms with a fake provider, including sensitive unknown values,
   known/no-op cases, type mismatches, dependency cycles, repeated references,
   and a replacement first revealed by the final plan.
4. Test the pinned Cloudflare provider against a loopback-only fake API with
   synthetic credentials and tokens. A live Cloudflare deployment is separate
   evidence and is not required by this design.
5. Update the infrastructure how-to, schema coverage matrix and infrastructure
   skill. Run focused CUE, Rust, fake-provider and schema checks, then the full
   root flake check before requesting review or marking the PR ready.
