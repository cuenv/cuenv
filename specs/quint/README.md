# Quint specs

Models of cuenv protocols that fail only under concurrency, lost responses
and partial failures. TLC (through `quint verify --backend=tlc`) explores
every reachable state of each model's finite instance.

| Spec | Code it models | Checked in CI |
| --- | --- | --- |
| `object_state_cas.qnt` | object-store state backend: lock, conditional PUTs, retries (`crates/infrastructure/src/state/object/store.rs`) | `safety`, `unlockReportsTruth` |
| `task_cache.qnt` | layered task cache: local and remote CAS and action cache (`crates/cas`, `crates/cas-remote`, `crates/task-exec/src/cache.rs`) | `safety`, `fixes` |

Run what CI runs (the `cuenv-quint-specs` flake check, a few minutes):

```bash
nix build .#checks.x86_64-linux.cuenv-quint-specs -L --accept-flake-config
```

Or one spec, with quint on `PATH`:

```bash
quint verify task_cache.qnt --backend=tlc --tlc-config=tlc.json --invariants safety fixes
```

`tlc.json` caps TLC's JVM heap; quint's default is 8 GiB.

The models are written by hand from the code, so CI checks the model, not the
Rust. Change a model in the same pull request as the protocol it describes.
Each model has mutation switches (constants at the top). Setting one to
another value turns off one mechanism or restores an earlier, buggy version
of it; the invariants should then fail, which shows they still guard what
they are meant to. In `object_state_cas.qnt`, `RELEASE` selects how
`release()` reads a lock that disappeared after it sent its PUT: `"checked"`
is the code, while `"notHeld"` (0.68.0) and `"anyGone"` (the first fix,
which snapshotted another run's locked document) both fail
`unlockReportsTruth`.

This is model checking of small instances (two clients or machines, one record
or action, a few retries and faults), not a proof for every size.
