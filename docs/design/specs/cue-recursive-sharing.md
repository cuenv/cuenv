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

Later commits complete lets and inline fields before reading them (cue-lang/cue#4548), let an
instance say where `@embed` reads from, add `cue/provenance`, and stop the sharing check from reading a field pushed
down from a comprehension, which panicked; see
`2026-11-03-infrastructure-nested-output-references.md`.

Since cuenv 0.75.0 the fork is pinned at
[`806b303`](https://github.com/rawkode/cue/commit/806b30344b62), the head of
its `share-imported-typed-refs` branch (`crates/cuengine/go.mod`). That commit
extends typed-reference sharing to references into imported packages, so a
chain of projects whose tasks depend on the tasks of the projects they import
(`dependsOn: [content.tasks.articles]`, see
`2026-11-12-cross-project-references.md`) evaluates in linear time, as a
same-package chain already did. Measured on a chain of eight projects, each
with two tasks depending on both tasks of the next project through its import:
`cuenv task a --dry-run` in the first project, which loads and plans all eight,
takes 1.2 seconds with a debug build, while stock CUE 0.17.1 does not finish
`cue export` of that project within 120 seconds.
`a_deep_chain_of_projects_evaluates_without_exploding`
(`crates/cuenv/tests/task_references.rs`) generates that chain and fails on an
exponential regression; it is never checked in as a fixture, since plain
`cue export` of it is what explodes. `ci.schema-compat` exports a chain of
three projects (`tests/schema-compat/testdata/modules/task-references`) under
every supported CUE release.

The fork is now pinned at
[`badfad1`](https://github.com/rawkode/cue/commit/badfad1109de70ce104a0637d9d853cf7ead536d),
the next commit on the same branch, which evaluates the operands that
`Value.Expr` returns where the value settled; see
[Operands of a superseded disjunct](#operands-of-a-superseded-disjunct).

Plain `cue export` slows with what stands behind an imported task, not only
with long chains. Take a project that depends on another project's task, where
that task has one dependency of its own. Stock CUE v0.18.0-rc.1 exports it in
about 3.4 seconds, and v0.17.1 did not finish within 280 seconds. The fork
evaluates it at once.

A lighter type for `dependsOn` entries, `{cuenvRef!: {kind!: ...}, ...} |
[...]`, makes that project export in 0.05 seconds on every release, but loses
the fork's sharing. The reference is then no longer constrained by the
definition that constrains the task, so a same-package chain of depth 7 with
fan-out 2 runs out of memory under the bridge, where it took 0.34 seconds.
Stock CUE runs out of memory on that chain with either type.

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

## Operands of a superseded disjunct

A project with a matrix entry over a task that is reached through a `let`, and
that has a dependency, ran out of memory under cuenv 0.74.0 and 0.75.1, while
stock `cue export` (v0.17.1) of it takes about 1.2 seconds:

```cue
let _tasks = tasks
ci: pipelines: default: tasks: [{type: "matrix", task: _tasks.build, matrix: arch: ["x"]}]
tasks: a: {command: "true"}
tasks: build: {command: "true", dependsOn: [tasks.a]}
```

A task typed with the union, `tasks: build: schema.#TaskNode & {...}`, does
the same without a pipeline. Evaluation was not at fault: the fork's `cue
export` takes 0.2 seconds. The bridge records the reference of every field
it exports (`referenceWalk` in `crates/cuengine/metadata.go`), and for a field
that neither the instance's source nor `Value.ReferencePath` settles, it splits
the field's conjuncts with `Value.Expr`, which evaluates each operand again.
For `stdout` of the matrix task, `#TaskOutputRef & {cuenvTask: _name, ...}`,
that evaluation did not end.

Both declarations give the task two disjunctions: `#TaskNode` from
`#MatrixTask.task` (or from the declaration itself), and the pattern of
`tasks`, which a reference through a `let` brings along with the value. A
disjunction clones the node it is defined on, and each further disjunction
clones the clones of the one before. A field added to the first clone keeps
that clone as the environment of its conjuncts, and evaluation leaves the
clone unfinished once it has cloned it again. `Value.Expr` evaluated the
operands of `stdout` in that clone, so the reference to `_name` resumed it. The
clone's disjunctions were processed again, then those of each task its
`dependsOn` reaches, without the sharing that evaluation used. The work grew by
a factor of about 3.5 per task of the chain.

Stock CUE has the same flaw, on v0.16.1 and on upstream master
(`e1a8dc4`, 2026-11-12). The resumed clone also lacks the conjuncts that later disjunctions
contribute, so `Value.Expr` returns wrong operands:

```cue
#A: {kind: "a", _n: string | *"default", out: {name: string} & {name: _n}}
#B: {kind: "b"}
x: #A | #B
x: {_n: "set", kind: "a"} | {kind: "q"}
```

`x.out.name` is `"set"`, but the second operand that `Value.Expr` returns for
`x.out` has `name: _`.

The fork's
[`badfad1`](https://github.com/rawkode/cue/commit/badfad1109de70ce104a0637d9d853cf7ead536d)
adds `adt.SettledEnvironment`. It replaces each vertex of an environment that
is not finalized and lies in a superseded disjunct with the vertex that
evaluation settled on at the same position. That vertex is the arc with the same
label of the vertex settled on for its parent, followed through the disjunct
it resolved to. A vertex without such an arc, or whose replacement is not
finalized, is kept. `Value.Expr` evaluates its operands in that environment, and
the operand above has `name: "set"`.

| Measurement                                                                         | Before                       | After                       |
| ----------------------------------------------------------------------------------- | ---------------------------- | --------------------------- |
| `cuenv task --path app` on the matrix project, `ulimit -v 8000000`                  | out of memory after 3.6 s    | 0.22 s, 168 MB              |
| `cuenv task build --dry-run`, matrix over a chain of 8 tasks                        | out of memory after 4 s      | 0.25 s, 199 MB              |
| `Value.Expr` on the matrix task's `out`, schema-free chain of depth D (fork test)   | 206 unifications at D = 2    | 2 unifications, D = 2 to 32 |

`TestSettledEnvironmentComplexity` (`internal/core/adt/settled_env_test.go`)
and `TestExprOperandsOfSupersededDisjunct` (`cue/types_test.go`) in the fork
fail without the change. In cuenv,
`crates/cuenv/tests/superseded_disjunct_evaluation.rs` plans a matrix over a
task reached through a `let` and a task typed `schema.#TaskNode &`, each with a
chain of eight dependencies, and stops a run that exceeds 30 seconds.
Apart from these tests, the fork's suite is unchanged: no golden file
changes. `internal/vcs` and `exp_gengotypes` fail in a scratch checkout with or
without the change, as they depend on the VCS layout around the checkout.

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
