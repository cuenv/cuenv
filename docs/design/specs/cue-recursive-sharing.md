# CUE recursive sharing measurements for 0.57.3

cuenv pins [fork commit 4cee740](https://github.com/rawkode/cue/commit/4cee740431d059398b71ce4c22906300c196b212),
which backports recursive-definition sharing onto CUE 0.16.1. The bridge uses
Go 1.26. The fork preserves stable CUE semantics rather than adopting CUE master.

**Release qualification is blocked.** The generic regression below establishes
the narrower direct-definition optimization. It does not establish that the
full typed cuenv schema is fixed. The release PR remains draft while that gap is
investigated; green CI exercises the current untyped workaround schema.

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

These evaluation-only controls use the current untyped `dependsOn` schema,
Go 1.26.3, `GOMAXPROCS=2`, and CUE module language `v0.14.1`. Depth D means
nodes 0 through D, with D dependency edges in a chain. A diamond has two nodes
per layer, each depending on both nodes in the previous layer.

| Graph         | Depth 5 unifications | Depth 12 unifications | Depth 20 unifications |
| ------------- | -------------------- | --------------------- | --------------------- |
| Task chain    | 1,986                | 3,834                 | 5,946                 |
| Group chain   | 2,761                | 5,512                 | 8,656                 |
| Image chain   | 725                  | 1,152                 | 1,640                 |
| Service chain | 1,946                | 3,668                 | 5,636                 |
| Diamond       | 3,670                | 7,534                 | 11,950                |

The evaluation measurement loads the instance, builds it, and validates it with
`cue.Concrete(false)`. It excludes JSON export, metadata collection and dependency
subtree traversal. CLI controls use `CUE_STATS_FILE=<json> cue vet .` from each
fixture directory.

## Pre-#515 typed schema

The same graph shapes were measured against all 33 schema files from commit
`092d0c9665079cd7945ad3205b10952d91b8a72b`. Three cases complete and twelve hit
the 58-second Go test timeout, with a separate 60-second process bound.

| Shape | Depth | CUE elapsed (s) | Process wall (s) | Unifications | Status |
| --- | ---: | ---: | ---: | ---: | --- |
| task | 5 | — | 58.9503 | — | timeout (58 s test / 60 s outer) |
| task | 12 | — | 59.0905 | — | timeout (58 s test / 60 s outer) |
| task | 20 | — | 59.2942 | — | timeout (58 s test / 60 s outer) |
| group | 5 | 21.254757 | 22.0147 | 2,566,035 | pass |
| group | 12 | — | 59.8194 | — | timeout (58 s test / 60 s outer) |
| group | 20 | — | 59.5805 | — | timeout (58 s test / 60 s outer) |
| image | 5 | 0.298406 | 0.3107 | 20,870 | pass |
| image | 12 | — | 58.6006 | — | timeout (58 s test / 60 s outer) |
| image | 20 | — | 58.6435 | — | timeout (58 s test / 60 s outer) |
| service | 5 | 2.988330 | 3.0834 | 153,444 | pass |
| service | 12 | — | 59.0599 | — | timeout (58 s test / 60 s outer) |
| service | 20 | — | 58.9697 | — | timeout (58 s test / 60 s outer) |
| diamond | 5 | — | 59.2198 | — | timeout (58 s test / 60 s outer) |
| diamond | 12 | — | 59.2008 | — | timeout (58 s test / 60 s outer) |
| diamond | 20 | — | 58.8438 | — | timeout (58 s test / 60 s outer) |

Elapsed evaluation time and final counters are unavailable for timeouts. Their
output is `panic: test timed out after 58s`; the process wall time includes test
shutdown. Successful evaluation times cover loading, building and validation,
without JSON export. These results support retaining #515's untyped `dependsOn`
schema for 0.57.3.

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

## Original master versus backport

A fresh comparison uses untouched source archives for original master commit
`304ad24c7bcc1d48fa47c2e65e98cb25d21955e0` and backport commit
`4cee740431d059398b71ce4c22906300c196b212`. Both receive identical complete
pre-#515 schemas and task fixtures, with module language `v0.14.1`, Go 1.26.3
and `GOMAXPROCS=2`. Independently compiled CLI `cue vet .` and the evaluation
helper agree on the following outcomes:

| Plain task chain depth | Original master | Stable backport |
| --- | --- | --- |
| 1 | Invalid: `name: field not allowed` and task-field errors | Pass, 68,645 unifications |
| 3 | Timeout | Timeout |
| 5 | Timeout | Timeout |

The helper's Go test guard is 18 seconds; each process has a separate 20-second
bound. Timeout stacks are inside `BuildInstance`, before validation finishes and
without JSON export. These results do not establish that the backport introduced
the full-schema slowdown: original master also exhibits it, alongside its
compatibility failures.

Adding explicit `schema.#Task &` to every node improves the depth-3 case but
does not establish linear growth. Independent CLI counts are:

| Explicit task chain depth | Original master | Stable backport |
| --- | --- | --- |
| 3 | Invalid, 30,394 unifications | Pass, 32,216 unifications |
| 5 | Invalid, 812,504 unifications | Pass, 863,898 unifications |

The original regression uses a single definition in the task pattern and
dependency list. The actual schema combines task/image unions, naming fields,
and computed defaults. Passing the former does not prove the latter is fixed.
Tracing the full-schema depth-1 case shows the dependency disjunction calling
`unshare()` before the referenced task arrives. The surviving branch also lacks
the exact direct-definition evidence required by the optimization: the source
task comes from an embedded task alternative. None of the new repeated-definition
sharing checks succeed in that case. This identifies an uncovered union path;
it does not establish a safe optimization for it.

The [standalone reproducer](cue-recursive-sharing/recursive-union-computed-default.cue)
contains no imports, comprehensions or group child pattern. On the backport,
with module language `v0.14.1`, keeping only node `n0` takes 105 unifications;
keeping `n0` and `n1` takes 132; the complete three-node fixture hits the
18-second test timeout. In separate one-change controls on the larger
standalone precursor, removing the computed naming default, using a task-only
root constraint, or using a task-only dependency constraint allows depth 2 to
finish quickly. This is a diagnostic counterexample, not a proposed schema change.

The next performance gate must include a minimized regression for this shape
and the full typed-schema matrix. Any further optimization must preserve
defaults and closedness; satisfying one union alternative alone is insufficient
to skip the others, because another alternative can contribute defaults and
observable fields.

## Baseline regression fixtures and remaining limits

The four new Rust regression shapes also have baseline CLI fixture evidence.
With stock CUE 0.16.1, `cue export .` hits a 30-second bound using the pre-#515
schema. The same fixtures complete with the workaround schema:

| Fixture       | Pre-#515 schema | Workaround schema seconds |
| ------------- | --------------- | ------------------------- |
| Group, 12     | Timeout, 30 s   | 0.167                     |
| Image, 12     | Timeout, 30 s   | 0.056                     |
| Service, 12   | Timeout, 30 s   | 0.126                     |
| Diamond, 6    | Timeout, 30 s   | 0.219                     |

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
