# Quint specs

Models of cuenv protocols that fail only under concurrency, lost responses
and partial failures.

| Spec | Code it models | TLC invariants (`checks.quint`) | Model-based tests (`checks.quint-mbt`) |
| --- | --- | --- | --- |
| `object_state_cas.qnt` | object-store state backend: lock, conditional PUTs, retries (`crates/infrastructure/src/state/object/protocol.rs`) | `safety`, `unlockReportsTruth` | `state::object::mbt` in `cuenv-infrastructure` |
| `task_cache.qnt` | layered task cache: local and remote CAS and action cache (`crates/cas`, `crates/cas-remote`) | `safety`, `fixes` | `mbt::` in `cuenv-cas-remote`'s `reapi_roundtrip` test |

Two checks, two questions:

- **Is the design safe?** `cuenv-quint-specs` runs TLC, which explores every
  reachable state of each model's finite instance.
- **Does the code do what the model says?** `cuenv-quint-mbt` replays random
  traces of each model against the real code and compares model and code
  state after every step. The state-store test drives the real write
  protocols; the cache test drives the real layered stores against an
  in-process REAPI server with the model's faults applied for real.

```bash
nix build .#checks.x86_64-linux.cuenv-quint-specs -L --accept-flake-config
nix build .#checks.x86_64-linux.cuenv-quint-mbt -L --accept-flake-config
```

One spec by hand, with quint on `PATH`:

```bash
quint verify task_cache.qnt --backend=tlc --tlc-config=tlc.json --invariants safety fixes
```

`tlc.json` caps TLC's JVM heap; quint's default is 8 GiB.

## Writing for the model-based tests

`quint-connect` names each step by the action that is a direct branch of the
last `any`, and reads that action's `nondet` picks. So every alternative of
`step` is its own named action whose `nondet`s are declared at its top, with
no anonymous `any` inside. Helpers are `pure def`s. The state-store model's
client states are the protocol states in `protocol.rs`, one to one.

`task_cache.qnt` also defines `corruptionStep` and `localFailureStep`: the
same machines under a narrower environment. Random runs of `step` rarely
line up a corrupt remote output with a machine that must read it, or a
failed local write with a remote hit; these steps make those paths common.

## Keeping them honest

The models are written by hand. Change a model in the same pull request as
the protocol it describes; `checks.quint-mbt` fails if they disagree. Each
model has mutation switches (constants at the top). Setting one to another
value turns off one mechanism or restores an earlier, buggy version of it,
and the invariants should then fail. In `object_state_cas.qnt`, `RELEASE`
selects how a release reads a lock that disappeared after it sent its PUT:
`"checked"` is the code, while `"notHeld"` (0.68.0) and `"anyGone"` (the
first fix, which snapshotted another run's locked document) both fail
`unlockReportsTruth`.

The model-based tests sample random traces, so a rare interleaving is not
reached on every run: reverting the release check is caught by about two
seeds in three. Each such bug also has a deterministic regression test (for
the release race, `a_release_that_loses_to_another_run_is_not_reported_released`
against the fake S3 server).

This is model checking of small instances (two clients or machines, one
record or action, a few retries and faults), not a proof for every size.
