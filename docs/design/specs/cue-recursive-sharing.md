# CUE typed reference sharing for 0.57.3

cuenv pins the CUE fork at
[`5a1f958`](https://github.com/rawkode/cue/commit/5a1f958250d657f19d94121d0070ddd1faca6805),
based on CUE 0.16.1, and builds the bridge with Go 1.26. The fork carries three
evaluator commits on top of 0.16.1:

- [`4cee740`](https://github.com/rawkode/cue/commit/4cee740431d059398b71ce4c22906300c196b212)
  backports recursive-definition sharing.
- [`183f265`](https://github.com/rawkode/cue/commit/183f265db67b96a257846cc77c9db23bd6a95ca1)
  shares forward references into closed definitions.
- [`5a1f958`](https://github.com/rawkode/cue/commit/5a1f958250d657f19d94121d0070ddd1faca6805)
  shares typed references to values of the type.

The schema keeps its types. `dependsOn` on `#Task`, `#TaskGroup`,
`#ContainerImage` and `#Service`, `#Pipeline.tasks`, `#MatrixTask.task` and
`#Watch.rebuild` are typed with `#TaskNode` unions. #515 had made `dependsOn`
untyped to avoid the exponential evaluation described below; this release
restores the types.

## Typed references

A typed dependency unifies a reference with a union of definitions:

```cue
#Task: dependsOn?: [...(#TaskNode | #ContainerImage)]
tasks: deploy: dependsOn: [build]
```

Stock CUE never shares such a reference. The disjunction disables structure
sharing, so `build` is replayed into every disjunct of the union. Each replayed
copy unifies the typed references inside `build` in turn. A failing branch,
such as `build & #TaskGroup`, is evaluated completely before it fails.
Evaluation is exponential in the depth of the dependency graph, with a large
base: one edge cost 55,218 unifications.

The fork shares the reference with `build` when the type provably resolves to
it:

- `build` is final, rooted, and not the node itself or one of its ancestors.
- Each other conjunct of the node is a type made only of definitions: a
  definition, or a disjunction without defaults of definitions.
- `build` was constrained by one of those definitions, with the definition's
  closedness enforced at `build`. That means a direct reference with typo
  checking enabled, not embedded within `build`. Unifying `build` with that
  definition again repeats a conjunct.
- Each other definition of the type cannot be unified with a definition that
  constrains `build`. `#Task & #TaskGroup` fails on `type`, so
  `build & #TaskGroup` fails too: `build` is an instance of `#Task`.

The disjunction then has one result, `build` itself, and the node shares it.
Whether two definitions can be unified is computed once per pair, by fully
evaluating their unification on a detached vertex. It does not depend on where
a reference occurs. Anything that does not meet these conditions takes the
usual path.

A target that has not been evaluated yet is completed first, as evaluation would
complete it on demand. The fork does this only after it has established that the
other conjuncts are such types. It never completes the node itself, an ancestor,
or a value under evaluation.

## Declaration-order sharing

`183f265` fixes a separate guard. CUE shares a reference into a recursively
closed destination only if the target is already marked `ClosedRecursive`. A
field is given that mark when it is initialized under a recursively closed
parent, so an unevaluated target failed the guard even though it would be
closed. A project that declared `ci: pipelines` before `tasks` copied every
referenced task. The fork treats an unprocessed target under a recursively
closed parent as closed for this guard, which is the flag that initialization
would set.

## Measurements

These evaluation-only measurements use the
[fixture generator](cue-recursive-sharing/fixtures.py) and
[counter helper](cue-recursive-sharing/eval_test.go), Go 1.26.3, `GOMAXPROCS=2`
and a 30-second bound. Each fixture uses the repository schema. Depth D means
nodes 0 through D, with D dependency edges in a chain. A diamond has two nodes
per layer, each depending on both nodes in the previous layer. "Reverse"
declares each node before the node it depends on.

| Shape   | Depth | Stock 0.16.1 | Fork, ascending | Fork, reverse |
| ------- | ----: | -----------: | --------------: | ------------: |
| task    |     5 |      timeout |           2,229 |         2,229 |
| task    |    20 |      timeout |           6,009 |         6,009 |
| group   |     5 |    2,566,035 |           2,844 |         2,844 |
| group   |    20 |      timeout |           8,514 |         8,514 |
| image   |     5 |       20,870 |             764 |           764 |
| image   |    20 |      timeout |           1,199 |         1,199 |
| service |     5 |      153,444 |           2,655 |         2,655 |
| service |    20 |      timeout |           6,000 |         6,000 |
| diamond |     5 |      timeout |           3,769 |         3,769 |
| diamond |    20 |      timeout |          11,419 |        11,419 |

Cells are unifications. Every fixture evaluates in under 0.3 seconds on the
fork. Before #515, the typed schema timed out on stock CUE for 12 of the 15
ascending fixtures. The untyped schema evaluated `task-20` in 5,946 unifications.

### waddle-social/waddle

The table shows `cuenv task` wall time for each project of
[waddle-social/waddle](https://github.com/waddle-social/waddle) at `b70a3f8`,
which pins the cuenv 0.57.2 schema with typed `dependsOn`. Times were measured
on a shared four-core container.

| Project                       | Stock 0.16.1 | Fork, 0.57.2 schema | Fork, 0.57.3 schema |
| ----------------------------- | -----------: | ------------------: | ------------------: |
| `server`                      |       2.98 s |              0.36 s |              0.39 s |
| `infrastructure/waddle.cloud` |       0.31 s |              0.12 s |              0.12 s |
| `colony`                      |       3.96 s |              0.11 s |              0.12 s |
| `chat`                        |      39.48 s |              0.22 s |              0.20 s |
| `apps/linux`                  |       0.24 s |              0.13 s |              0.11 s |
| `apps/android`                |       2.21 s |              0.16 s |              0.17 s |
| `website`                     |      23.23 s |              0.13 s |              0.17 s |

The stock column is `cue vet -c=false` with the 0.57.2 schema. For `chat`,
`cue vet` counts 1,109,564 unifications on stock CUE and 6,568 on the fork.
`cuenv info`, which evaluates the whole module, takes 0.76 seconds.

## Compatibility

`cue export` of all eight waddle `env.cue` files is byte-identical on stock CUE
0.16.1 and on the fork. The repository root and the 30 example projects with an
`env.cue` export identically on stock CUE with the 0.57.3 untyped schema and on
the fork with the typed schema.

In the fork, the full Go suite, `go vet ./...`, the race suite for
`internal/core/adt` and `cue`, and staticcheck for `internal/core/adt` pass. The
exception is `cmd/cue` `fmt_issue1791`, which checks a `chmod 000` permission
error and cannot fail when run as root. It fails the same way without the
fork's changes. Golden changes are confined to evaluator stats and to references
that are now shared, such as `~(t1.foo)` in place of a copy of its value.
`TestTypedReferenceSharingComplexity` and `TestUntypedForwardSharingComplexity`
check declaration-order linearity and fail without the changes.
`cue/testdata/eval/typedsharing.txtar` covers embeddings, defaults,
overlapping definitions, closedness errors, forward references and
self-references. Its errors match those of the evaluator without the change.

Export still writes every dependency's full subtree. Projects with heavy fan-in
remain slower to export than to evaluate.

### Reproduction

Run from the repository root in the Nix development shell with Go 1.26.3:

```sh
python3 docs/design/specs/cue-recursive-sharing/fixtures.py "$PWD" /tmp/cuenv-cue-fixtures
```

The counter helper resets CUE's global counters, starts the timer, calls
`load.Instances`, builds the single `cuenv` instance, and calls
`Validate(cue.Concrete(false))`. It reads `adt.TotalStats` after validation.
Copy it into `internal/cuenvbench` of a CUE checkout at the measured commit,
so that it can read CUE's internal counters, then compile and run it:

```sh
mkdir -p internal/cuenvbench
cp "$CUENV/docs/design/specs/cue-recursive-sharing/eval_test.go" internal/cuenvbench/
go test -c -o /tmp/cuenv-cue-eval.test ./internal/cuenvbench
GOMAXPROCS=2 EVIDENCE_ROOT=/tmp/cuenv-cue-fixtures/ascending/task-20 \
  timeout 60s /tmp/cuenv-cue-eval.test -test.run='^TestEvidence$' -test.timeout=58s
```

Replace the fixture path for the other shapes, depths and orders.
