# Nested infrastructure output references

Status: design and experiments in progress. The behavior described as a goal
below is not yet implemented.

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
the current limitation: an ordinary literal configuration passed, while five
nested descriptor cases failed with `config.token: expected string, got an
object`. No live Cloudflare service was used.

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
