# Hermetic Execution and CAS: Gap Analysis and Roadmap

**Date:** 2026-09-20
**Status:** Draft — planning only, no code changes
**Supersedes in practice:** the unimplemented half of
[ADR-0008](../../src/content/docs/decisions/adrs/adr-0008-hermetic-task-execution-cache.md)

## Summary

`cuenv` ships a content-addressed store, an action cache, and a schema field
called `hermetic`. None of the three currently does what its name implies:

- **`hermetic` is a no-op.** Every host task runs through
  `execute_task_non_hermetic` (`crates/task-exec/src/executor.rs:284-299`).
  The `hermetic` field only picks a working directory
  (`executor.rs:370`). ADR-0008's "fresh working directory pre-populated
  only with resolved inputs" was never built.
- **The CAS holds outputs only.** Input blobs are never ingested.
  `build_input_root_digest` (`crates/task-exec/src/cache.rs:570`) constructs a
  `Directory` tree out of hashes alone and throws it away. `merkle.rs` —
  the code that would ingest and re-materialize an input tree — is dead:
  `build_input_tree` and `materialize_input_tree` have no callers outside
  the crate's own tests.
- **The action key is not portable.** Ambient `HOME`, `USER`, `TERM`,
  `XDG_*` and `TMPDIR` are folded into `Command.environment_variables`
  (`cache.rs:176-180`) via `merge_with_system_hermetic`. Two developers,
  or a developer and a CI runner, can never share a cache entry.

The consequence is that we have the *shape* of a Bazel-style execution
substrate with none of its guarantees, and the pieces that would make it
competitive — remote cache, sandboxing, bounded storage, fast hashing — are
absent. This document inventories the gap honestly, picks the fights worth
having against moon and buck2, and sequences the work.

## 1. Inventory: what exists today

| Component | Location | State |
| --- | --- | --- |
| `Digest` (sha256 + size), REAPI-shaped | `crates/cas/src/digest.rs` | Solid |
| Canonical encoding | `digest.rs:56` | `serde_json` + `BTreeMap`; explicitly a placeholder for protobuf |
| REAPI-shaped messages | `crates/cas/src/message.rs` | `Action`, `Command`, `Directory`, `ActionResult`, `Platform`, `SymlinkNode` all modelled |
| `Cas` trait + `LocalCas` | `crates/cas/src/cas.rs` | Works; atomic install, digest verification on read, `EXDEV` fallback |
| `ActionCache` trait + `LocalActionCache` | `crates/cas/src/action_cache.rs` | Works; JSON entries, atomic persist |
| Merkle tree build/materialize | `crates/cas/src/merkle.rs` | Implemented, **unused** |
| Action construction | `crates/task-exec/src/cache.rs:114` | Inputs + command + env + platform + version |
| Cache hit materialization | `cache.rs:328` | File-by-file `fs::copy` into workdir |
| Cache record | `cache.rs:381` | Output files + stdout/stderr blobs |
| Input hashing | `crates/vcs/src/walker.rs` | `WalkHasher`: full sequential re-hash of every matched file |
| Runtime identity in key | `crates/cuenv/src/commands/task/mod.rs` | Nix locked runtime digest folded into `Platform` — genuinely good |
| Structured skip reasons | `cuenv_events::CacheSkipReason` | Good UX foundation |

The bones are right. `Platform` properties carrying the locked Nix runtime
digest is a real asset — it is the correct way to express toolchain identity
and it is something moon does not have.

## 2. Findings

Ranked by whether they block us from competing. Severity is about the
product, not the code.

### Blocking

**F1 — `hermetic` does not isolate anything.**
Tasks run in the project root with the full workspace visible. Any
undeclared read is invisible to the action key, so a recorded
`ActionResult` may be a function of files the key never saw. Every entry we
write today is potentially unsound. This is the reason a remote cache
cannot simply be bolted on: sharing unsound entries across machines turns a
local annoyance into a fleet-wide wrong-answer incident.
*Schema-coverage-matrix already concedes this: "filesystem hermeticity needs
status callouts" (`#Task` row).*

**F2 — Input blobs never enter the CAS.**
Without an ingested input root there is no remote execution, no
cold-cache reconstruction of an exec root, and no way to explain a cache
key after the fact. `merkle.rs` exists for exactly this and is unwired.

**F3 — The action key embeds ambient host state.**
`HOME`, `USER`, `LOGNAME`, `SHELL`, `TERM`, `COLORTERM`, `TMPDIR`,
`XDG_RUNTIME_DIR`, `XDG_*` and every `LC_*` go into the key. Cross-machine
hit rate is structurally zero. Env that reaches an action must be
*declared*, not inherited.

**F4 — No remote cache.**
Local only, under `$CUENV_CACHE_DIR` / `$XDG_CACHE_HOME/cuenv`. CI
runners start cold every time. This is the single feature users switch
build tools for, and moon has had it for years.

**F5 — Caching is off by default and opts out of the monorepo case.**
`#TaskCachePolicy.mode` defaults to `"never"` (`schema/tasks.cue:76`), and
even when enabled we skip on `EmptyInputs`, `NonPathRef`, and `RuntimeEnv`
(`cache.rs:138-161`). `NonPathRef` means **any task consuming another
task's output is uncacheable** — that is the central monorepo workflow.
`RuntimeEnv` means any task with `env:` is uncacheable.

### Serious

**F6 — `cuenv_version` in every key.**
`cuenv_version: env!("CARGO_PKG_VERSION")` invalidates the entire cache on
every release. Tolerable locally; catastrophic once a shared remote cache
exists. Needs an explicit `action_semantics_version` integer, bumped only
when execution semantics change.

**F7 — Action-result integrity is unchecked.**
`lookup` (`cache.rs:280`) returns a hit without verifying that the blobs it
references still exist. `materialize_hit` then writes files one at a time
and can fail halfway, leaving a half-restored workdir with no rollback.

**F8 — Output directories are not supported.**
`output_directories` is `Vec::new()` on both the `Command` and the
`ActionResult` (`cache.rs:191`, `cache.rs:427`). Only files matched by a
glob *at record time* are captured, and a cache hit never removes stale
files the previous build left behind.

**F9 — Hashing is the slow path.**
`WalkHasher` re-reads and re-hashes every matched file on every task on
every run, sequentially, with no stat cache, no git-index fast path and no
ignore-file filtering. It is the only `VcsHasher` implementation — the
trait name is aspirational. moon shells out to git plumbing for this;
buck2 uses a file watcher.

**F10 — Materialization copies.**
`get_to_file` is `fs::copy` plus a full re-hash of the destination
(`cas.rs:190-200`). Bazel and buck2 hardlink out of the CAS. On a large
output tree this is the difference between milliseconds and seconds, on
every hit.

**F11 — No garbage collection, no bounds, no tooling.**
The CAS grows without limit. There is no `cuenv cache` command surface at
all — no `stats`, `gc`, `prune`, `verify`.

### Worth fixing on the way past

**F12** — No single-flight: two concurrent tasks with the same action digest
both execute.
**F13** — `normalize_workdir` falls back to an absolute path when the workdir
is under neither the project nor the module root (`cache.rs:649-657`),
baking a machine-specific path into the key.
**F14** — Symlinks are silently dropped from input trees
(`merkle.rs:72`); `SymlinkNode` is modelled but always empty.
**F15** — Neither the `Action` nor the `Command` blob is ever stored, so a
cache miss cannot be explained or diffed against the previous run.
**F16** — Cached stdout/stderr replay as two blocks, losing interleaving.

## 3. Competitive position

**Be honest about what each competitor is.**

*moon* is a task runner with toolchain management. Its cache is a hash of
declared inputs plus toolchain versions; its remote cache speaks the Bazel
Remote Execution API v2 so it works against bazel-remote, BuildBuddy and
friends without bespoke infrastructure. It does **not** sandbox. Its
strength is ergonomics, affected-project detection, and the fact that
remote caching works out of the box.

*buck2* is a hermetic build system. Its strength is action-level
correctness — sandboxed execution, a CAS-backed input root, deferred
materialization, remote execution, dep files — and an incremental graph
(DICE) that most teams will never need to reason about. Its cost is that
you must author rules in Starlark.

**Where cuenv already wins:** CUE-typed configuration with real
unification, first-class environment and secret resolution, Nix runtime
identity folded into the action key, and a "run your existing commands"
UX that needs no rule authoring.

**Where cuenv loses today:** correctness (F1), cross-machine caching (F3,
F4), and speed (F9, F10).

**The fight worth picking:** buck2's *soundness* with moon's *ergonomics*.
Concretely — hermetic, sandboxed, content-addressed actions that you get by
writing `inputs` and `outputs` on an ordinary command, not by writing
rules. Speaking REAPI means we inherit the entire existing remote-cache and
remote-execution ecosystem instead of building one.

**The fight to decline:** we are not building a rule language, a Starlark
interpreter, a DICE-equivalent incremental engine, or our own remote
execution worker fleet. We are a *client* of the REAPI ecosystem and a
*better front end* than either competitor.

## 4. Target architecture

```
 env.cue ──► Action { command_digest, input_root_digest, platform, salt }
                │
                ├─► ActionCache.lookup ──hit──► verify blobs ──► materialize (hardlink)
                │        local → remote (layered, read-through)
                │
                └─miss─► ingest input root into CAS
                         materialize exec root  (hardlink from CAS)
                         run under sandbox tier (namespaces / sandbox-exec / dir)
                         collect declared outputs → CAS
                         ActionCache.update → local, async write-back to remote
                         project outputs into the workspace
```

Two invariants the current design lacks and the target must hold:

1. **Everything an action can read is in its input root.** Anything else is
   a bug, and the sandbox is how we make it fail loudly rather than
   silently poison the cache.
2. **The action key names everything that can change the result** — including
   the isolation tier it ran under. A `dir`-tier result must never satisfy
   a `strict`-tier lookup (see §6.2).

## 5. Roadmap

Each phase is independently shippable and has an exit criterion. Phases 0–2
are prerequisites for phase 3; do not reorder them, because shipping a
remote cache on top of unsound keys is worse than shipping nothing.

### Phase 0 — Stop lying (correctness, no new capability)

- Declared env only. Add `env.passthrough` to the task schema; drop
  `merge_with_system_hermetic` from the action key path. Ambient env may
  still reach a non-hermetic task, but it may not enter a key it did not
  declare. **(F3)**
- Replace `cuenv_version` with `action_semantics_version: u32`. **(F6)**
- Verify every referenced blob exists before declaring a hit; make
  materialization atomic (stage to a temp tree, rename into place). **(F7)**
- Store the `Action` and `Command` blobs in the CAS at key-computation
  time. **(F15)**
- Fix the absolute-path fallback in `normalize_workdir`: refuse to cache
  rather than bake a host path into the key. **(F13)**
- Either implement `hermetic` or mark it `partial` in the
  schema-coverage-matrix and the schema comment until phase 1 lands. It
  must not keep claiming isolation it does not provide. **(F1, partial)**

*Exit:* two machines with identical checkouts and toolchains compute
identical action digests for the same task.

### Phase 1 — Real hermetic execution

- Wire `merkle.rs`. Ingest the resolved input set into the CAS; materialize
  a per-action exec root under `$CACHE/exec/<action-digest>/`. **(F2)**
- Run with `cwd` = exec root and exactly the declared environment. Collect
  declared outputs *from the exec root*, ingest, then project into the
  workspace.
- Sandbox tiers, named and explicit:
  - `strict` — Linux user + mount + PID + network namespaces
    (`unshare`), read-only bind of the input root, tmpfs elsewhere.
  - `sandbox-exec` — macOS seatbelt profile: deny filesystem writes outside
    the exec root, deny network.
  - `dir` — directory isolation only (today's ADR-0008 model), for
    platforms and container environments where namespaces are unavailable.
  - `none` — explicit opt-out for tasks that must touch the real workspace
    (`bun install`, codegen writing back into the tree).
  Degradation must be **named and recorded**, never silent.
- Network off by default for cacheable tasks. A cached result from a task
  that could reach the network is not a cached result; it is a guess. This
  is a genuine differentiator over moon.
- Symlink support in the input tree. **(F14)**

*Exit:* a task that reads an undeclared file fails under `strict` instead
of silently producing a poisoned cache entry.

### Phase 2 — Fast and bounded

- `GitHasher`: use the git index (`ls-files -s` / `hash-object`) for tracked
  files, walk only what is dirty or untracked; honour ignore files. **(F9)**
- Persistent stat cache: `(path, mtime, size, inode) → digest`, invalidated
  on mismatch. **(F9)**
- Parallel hashing.
- Hardlink materialization from the CAS, with reflink (`FICLONE` /
  `clonefile`) where the filesystem supports it, copy as last resort. Skip
  the redundant post-copy re-hash on the hardlink path. **(F10)**
- `output_directories` as first-class `Tree` messages; a cache hit replaces
  the output tree rather than merging into whatever is there. **(F8)**
- Single-flight on action digest: in-process map plus a cross-process
  lockfile. **(F12)**
- `cuenv cache` command surface: `stats`, `gc` (LRU by access time against a
  size budget), `prune`, `verify`. **(F11)**

*Exit:* warm no-op run on a large monorepo is dominated by process spawn,
not by hashing or copying; the cache respects a configured size budget.

### Phase 3 — Remote cache

- Split the store traits into `LocalCas` / `RemoteCas` with a layered
  read-through, async write-back stack.
- Switch the canonical encoding from `serde_json` to REAPI protobuf. This
  is a breaking key change — land it together with an
  `action_semantics_version` bump, and land it *before* anyone depends on
  remote hit rates.
- gRPC client (tonic + `bazel-remote-apis`): `ContentAddressableStorage`,
  `ActionCache`, `ByteStream`, `Capabilities`. Day-one compatibility with
  bazel-remote, buildbarn, BuildBuddy, NativeLink and EngFlow.
- HTTP/object-store fallback (bazel-remote HTTP layout over S3/GCS/R2) for
  teams without a gRPC endpoint.
- Auth: bearer headers and mTLS; read-only credentials for untrusted PR
  builds so a fork cannot poison the shared cache.
- Only tasks that ran at tier `strict` or `sandbox-exec` write to the
  remote cache by default. **(F1, enforced)**

*Exit:* a cold CI runner gets hits from a developer's local build and vice
versa.

### Phase 4 — Remote execution and observability

- REAPI `Execute` / `WaitExecution`. `Platform` properties are already
  modelled, and the Nix runtime digest is exactly the right worker
  selector.
- Hybrid scheduling: race local against remote, take the first result.
- `cuenv task explain <task>` — print the action digest, the input tree,
  and a **diff against the last recorded action for that task**. This is
  the feature neither moon nor buck2 makes pleasant, and F15 is what makes
  it possible.
- `cuenv task verify --repeat N` — run a task N times, diff output digests,
  report non-determinism. Catches non-hermetic tasks before they poison a
  shared cache.
- Interleaved stdout/stderr replay. **(F16)**

## 6. Decisions to make before phase 0 lands

### 6.1 Default cache mode

`mode` currently defaults to `"never"`. moon caches by default. Once phases
0–1 make keys sound, the default should flip to `"read-write"` for tasks
that declare both `inputs` and `outputs`, and stay `"never"` otherwise.
That is opt-in by construction — declaring inputs *is* the opt-in — without
demanding a second ceremony.

**Recommendation:** flip it in the same release as phase 1, behind a
release-note callout. Flipping earlier means caching more unsound entries.

### 6.2 Sandbox tier belongs in the action key

If `dir`-tier and `strict`-tier results share a key, a CI runner without
user namespaces will happily serve weakly-isolated (possibly wrong) results
to everyone else. The tier must be a `Platform` property, which means
degrading from `strict` to `dir` produces a cache miss — correct, and
visibly so.

### 6.3 `hermetic: true` is already the schema default

The default is `true` today and nothing enforces it, so implementing it is
technically not a default change — but it *is* a behaviour change for every
existing user, since tasks that quietly depended on workspace access will
start failing. Options:

1. Implement and keep `true` default; ship a migration release where
   failures name the undeclared path they tried to read. **Recommended** —
   the guarantee is worth one noisy release, and a good error message makes
   the fix mechanical.
2. Flip the schema default to `false` and make hermeticity opt-in. Safer,
   but concedes the whole positioning: a tool whose hermetic mode is off by
   default is a task runner, not a competitor to buck2.
3. Auto-detect: hermetic only when `inputs` are declared. Muddy — the same
   field would mean different things depending on a sibling field.

### 6.4 Dependency outputs as inputs

`NonPathRef` currently disables caching for any task consuming a
`#TaskOutputRef` or `#ProjectReference`. ADR-0008 explicitly chose "no
implicit output injection". That choice is defensible for *materialization*
but wrong for *hashing*: a dependency's output digests are exactly what
should feed the consumer's input root. Resolve the reference to the
producer's recorded `ActionResult` digests and fold them in.

## 7. Schema sketch

Illustrative, not final. Every change here requires a
`docs/design/specs/schema-coverage-matrix.md` update per the project rules.

```cue
#SandboxTier: "none" | "dir" | "strict" | *"auto"

#Hermetic: {
	// Isolation tier. "auto" picks the strongest tier the host supports
	// and records the result in the action's platform properties.
	sandbox?: #SandboxTier
	// Network access. Off by default: a cacheable task that can reach the
	// network is not reproducible.
	network?: bool | *false
	// Host environment variables permitted into the action, by name.
	// Values are recorded in the action key.
	passthrough?: [...string]
}

#Task: {
	// bool retained for compatibility: true ⇒ {sandbox: "auto"}.
	hermetic?: bool | #Hermetic | *true
	outputs?: [...(string | #OutputDir)]
	// ...
}

#OutputDir: {dir: string}

// Module level
#Cache: {
	remote?: {
		endpoint:  string           // grpc://… or https://…
		instance?: string | *""
		// Untrusted builds read but never write.
		mode?: "read" | "read-write" | *"read"
		auth?: #CacheAuth
	}
	local?: {
		maxSize?: string | *"20GiB"  // GC budget
	}
}
```

## 8. Non-goals

- A rule authoring language. `command` + `inputs` + `outputs` is the
  interface, and it is the reason to choose cuenv over buck2.
- Our own remote execution worker implementation. Be a REAPI client; let
  buildbarn, NativeLink and BuildBuddy be the servers.
- A DICE-equivalent incremental graph. The action cache plus content
  addressing covers the value for the monorepo workloads we target.
- Replacing Nix. Nix stays the toolchain identity provider; its locked
  digest is already the best part of our platform key.

## 9. Risks

- **User namespaces are unavailable in many CI containers.** Mitigated by
  the named tier in §6.2: we degrade visibly and take the cache miss,
  rather than degrading silently and serving a bad hit.
- **The protobuf encoding switch invalidates every existing key.** Land it
  in phase 3 with a semantics-version bump, before remote hit rates matter
  to anyone.
- **Phase 1 breaks existing users' tasks.** Real and unavoidable if we want
  the guarantee. The mitigation is error quality, not scope reduction: name
  the undeclared path, suggest the `inputs` entry that would fix it.
- **Hermetic exec roots multiply disk usage.** Hardlinks from the CAS (phase
  2) make an exec root nearly free; do not ship phase 1 materialization by
  copy at monorepo scale without phase 2 following closely.
- **Scope.** Phases 3 and 4 are each larger than phases 0–2 combined.
  Phases 0–2 are worth shipping on their own merits even if remote work
  slips.

## 10. Validation

Per the project's validation strategy, each phase is a cross-crate runtime
behaviour change in task execution and caching, and therefore requires
`nix flake check -L --accept-flake-config` before review. Additionally:

- Phase 0: property tests that identical logical inputs on different hosts
  produce identical action digests.
- Phase 1: a fixture task that reads an undeclared file, asserted to fail
  under `strict` and to be reported under `dir`.
- Phase 2: benchmark the warm no-op run on a synthetic monorepo; assert GC
  respects the budget.
- Phase 3: integration test against a local `bazel-remote` container.
- Phase 4: `--repeat` determinism check run against the repo's own task
  graph in CI.
