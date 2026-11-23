# Quint specs

Models of cuenv protocols that fail only under concurrency, lost responses
and partial failures. TLC (through `quint verify --backend=tlc`) explores
every reachable state of each model's finite instance.

| Spec | Code it models | Checked in CI |
| --- | --- | --- |
| `object_state_cas.qnt` | object-store state backend: lock, conditional PUTs, retries (`crates/infrastructure/src/state/object/store/object_state_store.rs`), and the CLI's release retry (`crates/cuenv/src/commands/infrastructure/lock.rs`) | `safety`, `unlockReportsTruth` |
| `infrastructure_changes.qnt` | provider changes the state may not learn: in-flight markers, unrecorded-change files and `state recover` (`crates/infrastructure/src/engine/infrastructure_engine.rs`, `crates/infrastructure/src/unrecorded.rs`, `crates/infrastructure/src/in_flight.rs`) | `safety` |
| `task_cache.qnt` | layered task cache: local and remote CAS and action cache (`crates/cas`, `crates/cas-remote`, `crates/task-exec/src/cache.rs`) | `safety`, `fixes` |

Run what CI runs (the `cuenv-quint-specs` flake check, about a minute):

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
they are meant to. In `object_state_cas.qnt`,
`RELEASE = "trusting"` restores the CLI that reported a lock gone on a retried
release as "no longer held", and `READBACK_WRITER = false` confirms a
timed-out release without its writer nonce; both fail `unlockReportsTruth`.
In `infrastructure_changes.qnt`, writing the marker after the provider call,
not writing it, plans that ignore it, recovery that clears the marker first,
and recovery that clears another run's marker each fail `safety`.
In `task_cache.qnt`, each switch turns off one integrity mechanism of the
cache and fails `safety` or `fixes`.

This is model checking of small instances (two clients or machines, one record
or action, a few retries and faults), not a proof for every size.
