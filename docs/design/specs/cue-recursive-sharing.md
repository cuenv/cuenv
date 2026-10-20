# CUE recursive sharing measurements for 0.57.3

cuenv pins [fork commit 183f265](https://github.com/rawkode/cue/commit/183f265db67b96a257846cc77c9db23bd6a95ca1),
which builds on the recursive-definition sharing backport
[`4cee740`](https://github.com/rawkode/cue/commit/4cee740431d059398b71ce4c22906300c196b212)
for CUE 0.16.1. The bridge uses Go 1.26. The fork preserves stable CUE semantics
rather than adopting CUE master.

## Declaration-order sharing

CUE shares a reference instead of copying its target only when the guards in
`shareIfPossible` allow it. One guard refuses to share into a recursively closed
destination unless the target is already marked `ClosedRecursive`. A field is
given that mark when it is initialized under a recursively closed parent, so a
target that has not been evaluated yet fails the guard even though it will be
closed. Every cuenv reference crosses this guard, because projects unify with
`schema.#Project`. Whether a reference was shared therefore depended on whether
its target had already been evaluated:

- a pipeline declared before `tasks` copied every task it referenced;
- a task depending on a task declared later copied that task;
- a copied task re-evaluated its own references inside the copy, so the cost
  followed the whole dependency closure.

Commit `183f265` treats an unprocessed target whose parent is recursively
closed as closed for this guard. That is the flag initialization would set.
Values under open parents, such as `reg: {}` shared into `#def: sub: reg`, are
still copied. The fork's `TestUntypedForwardSharingComplexity` evaluates a
pipeline declared before a chain of untyped dependencies in both declaration
orders. It stays within `64 * (depth + 1)` unifications through depth 32 (285
unifications), and fails at depth 2 or 4 without the change. The fork's full Go
suite, `go vet ./...` and the ADT race suite pass. The one golden change shares
a reference already shared within its definition and lowers its counts.

The schema closes the other source of copies. `#Pipeline.tasks`,
`#MatrixTask.task` and `#Watch.rebuild` were typed as `#PipelineTask` or
`#TaskNode`. Like typed `dependsOn` entries before #515, every entry
re-evaluated the referenced task. They are now untyped, with the same scalar
and output-reference checks. Matrix entries are validated by a hidden
`#MatrixTask` unification.

### waddle-social/waddle

The table shows unifications for `cue vet -c=false .:cuenv` in each project of
[waddle-social/waddle](https://github.com/waddle-social/waddle) at `b70a3f8`,
with `GOMAXPROCS=2` and Go 1.26.3. The project pins the 0.57.2 schema.
"0.57.2" is that schema. "Main" is the 0.57.3 schema before this change.
"New" adds the untyped pipeline and rebuild entries.

| Project                       | 0.57.2, stock | Main, stock | Main, fork | New, fork |
| ----------------------------- | ------------: | ----------: | ---------: | --------: |
| `server`                      |       133,978 |      39,287 |     39,160 |    18,829 |
| `infrastructure/waddle.cloud` |         9,843 |       6,456 |      6,456 |     3,569 |
| `colony`                      |       125,772 |      12,358 |      4,034 |     2,867 |
| `chat`                        |     1,109,564 |      72,192 |     12,140 |     6,708 |
| `apps/linux`                  |         7,562 |       7,578 |      7,578 |     2,733 |
| `apps/android`                |        75,021 |      29,286 |     15,806 |     6,485 |
| `website`                     |       649,653 |      27,254 |      5,062 |     3,831 |

Wall times on the same container were 2.0 to 33.6 seconds for the 0.57.2
schema, with `chat` at 33.6 s and `website` at 20.3 s. With the main schema on
stock CUE, `chat` took 1.4 s. With the new schema on the fork, every project took
0.5 s or less, measured while a Cargo build was running.

The root `env.cue` uses `schema.#Base` and costs 311 unifications in every
configuration. Moving `chat`'s `tasks` block above `ci` gave 14,440
unifications on stock CUE with the main schema, against 72,192 in the original
order. The fork gives 12,140 in both orders. `cuenv task` took 0.17 to 1.17 seconds per project with the main schema and the
`4cee740` build, and 0.09 to 0.44 seconds with the new schema and `183f265`.

The 0.57.2 schema stays exponential on the fork. Its typed `dependsOn` entries
are unified with definition unions, which the sharing proof does not cover; see
[the four-way comparison](#four-way-comparison-and-root-cause).

## Generic recursive graphs

The fork's `TestRecursiveSharingComplexity` evaluates typed fan-in and disjunction
graphs in ascending, reverse and mixed declaration orders. Every order produces
the same counts below. The bound is `64 * (depth + 1)` unifications.

| Depth | Fan-in unifications | Disjunction unifications |
| ----- | ------------------- | ------------------------ |
| 2     | 31                  | 20                       |
| 4     | 51                  | 30                       |
| 8     | 91                  | 50                       |
| 16    | 171                 | 90                       |
| 32    | 331                 | 170                      |

Stock CUE 0.16.1 exceeds the bound in every order: fan-in reaches 347
unifications at depth 4, and disjunctions reach 4,058 at depth 8. The backport
passes through depth 32. These small generic graphs establish the sharing
regression independently of cuenv's schema.

## Current cuenv schema

These evaluation-only controls use the current untyped schema and fork
commit `183f265`. The fixtures declare dependencies before their dependents, so
the counts match `4cee740` except for service chains, which gain three
unifications from the `services.cue` schema change. The controls use
Go 1.26.3, `GOMAXPROCS=2`, and CUE module language `v0.14.1`. Depth D means
nodes 0 through D, with D dependency edges in a chain. A diamond has two nodes
per layer, each depending on both nodes in the previous layer.

| Graph         | Depth 5 unifications | Depth 12 unifications | Depth 20 unifications |
| ------------- | -------------------- | --------------------- | --------------------- |
| Task chain    | 1,986                | 3,834                 | 5,946                 |
| Group chain   | 2,761                | 5,512                 | 8,656                 |
| Image chain   | 725                  | 1,152                 | 1,640                 |
| Service chain | 1,949                | 3,671                 | 5,639                 |
| Diamond       | 3,670                | 7,534                 | 11,950                |

The evaluation measurement loads the instance, builds it, and validates it with
`cue.Concrete(false)`. It excludes JSON export, metadata collection and dependency
subtree traversal. CLI controls use `CUE_STATS_FILE=<json> cue vet .` from each
fixture directory.

## Pre-#515 typed schema

The same graph shapes were measured against all 33 schema files from commit
`092d0c9665079cd7945ad3205b10952d91b8a72b`. Three cases complete and twelve hit
the 58-second Go test timeout, with a separate 60-second process bound.

| Shape   | Depth | CUE elapsed (s) | Process wall (s) | Unifications | Status                           |
| ------- | ----: | --------------: | ---------------: | -----------: | -------------------------------- |
| task    |     5 |               — |          58.9503 |            — | timeout (58 s test / 60 s outer) |
| task    |    12 |               — |          59.0905 |            — | timeout (58 s test / 60 s outer) |
| task    |    20 |               — |          59.2942 |            — | timeout (58 s test / 60 s outer) |
| group   |     5 |       21.254757 |          22.0147 |    2,566,035 | pass                             |
| group   |    12 |               — |          59.8194 |            — | timeout (58 s test / 60 s outer) |
| group   |    20 |               — |          59.5805 |            — | timeout (58 s test / 60 s outer) |
| image   |     5 |        0.298406 |           0.3107 |       20,870 | pass                             |
| image   |    12 |               — |          58.6006 |            — | timeout (58 s test / 60 s outer) |
| image   |    20 |               — |          58.6435 |            — | timeout (58 s test / 60 s outer) |
| service |     5 |        2.988330 |           3.0834 |      153,444 | pass                             |
| service |    12 |               — |          59.0599 |            — | timeout (58 s test / 60 s outer) |
| service |    20 |               — |          58.9697 |            — | timeout (58 s test / 60 s outer) |
| diamond |     5 |               — |          59.2198 |            — | timeout (58 s test / 60 s outer) |
| diamond |    12 |               — |          59.2008 |            — | timeout (58 s test / 60 s outer) |
| diamond |    20 |               — |          58.8438 |            — | timeout (58 s test / 60 s outer) |

Elapsed evaluation time and final counters are unavailable for timeouts. Their
output is `panic: test timed out after 58s`; the process wall time includes test
shutdown. Successful evaluation times cover loading, building and validation,
without JSON export. These results support retaining #515's untyped `dependsOn`
schema for 0.57.3.

### Four-way comparison and root cause

The typed timeouts do not come from a gap in the backport or from a benchmark
problem. The same fixtures and counter helper were compiled with Go 1.26.3
against four CUE revisions and run with `GOMAXPROCS=2` and a 30-second bound:

- stock CUE 0.16.1: `6d609d768`
- the 0.16.1 backport: `4cee74043`
- fork master without the fix: `4627d40b6`, which is upstream `06f65aeaf` plus
  two `tools/flow` commits
- fork master with the original fix: `304ad24c7`

| Typed fixture | Stock 0.16.1 |  Backport |    Master | Master + fix |
| ------------- | -----------: | --------: | --------: | -----------: |
| task, 1       |       68,645 |    68,645 |    67,111 |       67,111 |
| task, 2       |      timeout |   timeout |   469,012 |      469,012 |
| group, 2      |      176,459 |   176,459 |   172,447 |      172,447 |
| group, 4      |    1,163,403 | 1,163,403 | 1,136,667 |    1,136,667 |
| image, 5      |       20,870 |    20,870 |    20,356 |       20,356 |
| service, 5    |      153,444 |   153,444 |   147,990 |      147,990 |
| diamond, 1    |      272,262 |   272,262 |   266,044 |      266,044 |

The table lists unifications. The backport's counts equal stock's exactly, and the
original fix's counts equal the master baseline's. Neither changes cuenv's
typed schema. Every other typed fixture times out on all four revisions. On the
current untyped schema, all seven fixtures checked (task at depths 5 and 20,
group, image and service at depth 20, and the diamond at depths 5 and 20) give
identical unification and disjunct counts on stock and on the backport. The
fork therefore has no measured effect on either cuenv schema.

The fix proves containment only when the referenced value's declaration names
the same definition that the reference is unified with. A reduced model shows
the boundary. It uses `#N: #A | #B | #Seq`, with `dependsOn?: [...#N]` on `#A`,
and a chain of `#A` tasks:

| `tasks` declaration                                  |    Depth 12, stock | Depth 12, backport |
| ---------------------------------------------------- | -----------------: | -----------------: |
| `[string]: #N`                                       |            171,987 |                261 |
| `[string]: #A`                                       |             57,443 |             57,443 |
| `[string]: #A & {_p: ""}` through a named definition |             57,443 |             57,443 |
| `[n=string]: ((#A \| #B) & {_p: "", _s: n}) \| #Seq` | timeout at depth 8 | timeout at depth 8 |

The pre-#515 schema never matches exactly. Tasks are declared as
`((#Task | #TaskGroup) & {_cuenvPrefix, _cuenvSelf}) | #TaskSequence`, and
references are unified with `#TaskNode | #ContainerImage`. To make the typed
schema linear, the evaluator would have to prove containment for each
disjunct of a definition disjunction, inside disjunction overlays. The current
fix explicitly refuses to share in those overlays. That is new evaluator work, not a backport
correction.

Fork master also rejects these fixtures, and cuenv's own root `env.cue`, with
`field not allowed` errors that stock 0.16.1 does not raise. Root evaluation
reports `ci.contributors: field not allowed`. A future CUE upgrade must
resolve this before it can ship.

### Reproduction

Run from the repository root in the Nix development shell with Go 1.26.3.
The [fixture generator](cue-recursive-sharing/fixtures.py) creates the exact 30
measured fixtures, including their local schema copies and explicit module
language `v0.14.1`. Each chain has nodes 0 through D; the diamond has two nodes
per layer. The task fixture contains only commands and dependency references.
Image and service dependencies use local `nN` references.

```sh
python3 docs/design/specs/cue-recursive-sharing/fixtures.py "$PWD" /tmp/cuenv-cue-fixtures
```

The [counter helper](cue-recursive-sharing/eval_test.go) resets CUE's global
counters, starts the timer, calls `load.Instances`, builds the single `cuenv`
instance, and calls `Validate(cue.Concrete(false))`. It reads `adt.TotalStats`
after validation. Compile it before timing, using the pinned fork and the stable
bridge dependency versions:

```sh
cuenv_bench=$(mktemp -d)
cp crates/cuengine/go.mod crates/cuengine/go.sum "$cuenv_bench/"
cp docs/design/specs/cue-recursive-sharing/eval_test.go "$cuenv_bench/"
(
  cd "$cuenv_bench"
  go mod edit -module=cuelang.org/go/cuenv-bench -go=1.25.0
  GOPROXY=direct GOTOOLCHAIN=auto go mod tidy
  go test -c -o /tmp/cuenv-cue-eval.test
)
GOMAXPROCS=2 EVIDENCE_ROOT=/tmp/cuenv-cue-fixtures/typed/task-5 \
  timeout 60s /tmp/cuenv-cue-eval.test -test.run='^TestEvidence$' -test.timeout=58s
```

The helper's module path permits access to CUE's internal counters. Its
`go 1.25.0` declaration matches the measured helper; the compiler is Go 1.26.3.
Replace the fixture path for the other shapes and depths, or use `untyped` for
current controls. The parent process used to collect the table enforced 60
seconds and recorded child CPU time and wall time. Wall times were collected
alongside other verification workloads and are observations, not latency
guarantees.

For independent CLI control counts, run the following in the fixture directory
with the fork's compiled CLI:

```sh
GOMAXPROCS=2 CUE_STATS_FILE=/tmp/cue-stats.json /path/to/cue vet .
```

CLI control counts match the counter helper. Neither command exports dependency
subtrees.

## Compatibility and export limits

The repository root and all 34 example projects produce identical complete JSON
envelopes to stock CUE 0.16.1, including metadata and dependency references.
The fork's full Go suite, full ADT race suite, repeated concurrent-read tests and
`go vet ./...` pass. New semantic regression tests also pass on the stock
baseline; the complexity test fails there as intended.

The four new Rust regression shapes also have baseline CLI fixture evidence.
With stock CUE 0.16.1, `cue export .` hits a 30-second bound using the pre-#515
schema. The same fixtures complete with the workaround schema:

| Fixture     | Pre-#515 schema | Workaround schema seconds |
| ----------- | --------------- | ------------------------- |
| Group, 12   | Timeout, 30 s   | 0.167                     |
| Image, 12   | Timeout, 30 s   | 0.056                     |
| Service, 12 | Timeout, 30 s   | 0.126                     |
| Diamond, 6  | Timeout, 30 s   | 0.219                     |

These baseline measurements use the CLI fixtures. The Rust harness was executed
against the final fork and current schema.

Export still writes every dependency's full subtree. Evaluation counts above do
not measure that expansion. The CLI regression diamond uses depth 6; its
end-to-end test includes export and Rust decoding and enforces a 30-second bound.

The release's full Nix gate also exposed sandbox assumptions in existing tests.
The secret-command fixture now resolves `printf` through `PATH`, and the scoped
task-directory fixture explicitly supplies the host `PATH` to its shell command.
The nextest derivation supplies the Nix CA bundle because constructing an HTTP client
loads native certificate roots even for localhost mocks. An empty CA store
reproduces the GCP client-construction failures; supplying the bundle passes all
five GCP resolver tests.
