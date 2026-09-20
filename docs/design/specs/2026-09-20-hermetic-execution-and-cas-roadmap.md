# Hermetic Execution and CAS: Gap Analysis and Roadmap

**Date:** 2026-09-20
**Status:** Phase 0 implemented; phases 1–4 planned
**Supersedes in practice:** the unimplemented half of
[ADR-0008](../../src/content/docs/decisions/adrs/adr-0008-hermetic-task-execution-cache.md)

## Summary

> **Reading note.** §§1–2 describe the state this document was written
> against. Phase 0 has since shipped and fixed F3, F6, F7, F13 and F15; see
> §5 for exactly what changed. The rest of the analysis stands.

`cuenv` ships a content-addressed store, an action cache, and a schema field
called `hermetic`. None of the three did what its name implies:

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
product, not the code. Findings marked **[fixed in phase 0]** describe the
state this document was written against; see §5 for what replaced them.

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

**F3 — The action key embeds ambient host state.** *[fixed in phase 0]*
`HOME`, `USER`, `LOGNAME`, `SHELL`, `TERM`, `COLORTERM`, `TMPDIR`,
`XDG_RUNTIME_DIR`, `XDG_*` and every `LC_*` go into the key. Cross-machine
hit rate is structurally zero. Env that reaches an action must be
*declared*, not inherited.

**F4 — No remote cache.**
Local only, under `$CUENV_CACHE_DIR` / `$XDG_CACHE_HOME/cuenv`. CI
runners start cold every time. This is the single feature users switch
build tools for, and moon has had it for years.

**F5 — Caching is off by default and opts out of the monorepo case.**
*[same-project half fixed]*
`#TaskCachePolicy.mode` defaults to `"never"` (`schema/tasks.cue:76`), and
even when enabled we skip on `EmptyInputs`, `NonPathRef`, and `RuntimeEnv`
(`cache.rs:138-161`). `NonPathRef` means **any task consuming another
task's output is uncacheable** — that is the central monorepo workflow.
`RuntimeEnv` means any task with `env:` is uncacheable.

`RuntimeEnv` is gone: §6.6 fingerprints secrets, so a task with `env:` is
cacheable. Same-project `#TaskOutput` references are gone too (§6.7).
Cross-project `#ProjectReference` still skips, because the producer's outputs
live under a different project root than the input hasher (see §6.7).

### Serious

**F6 — `cuenv_version` in every key.** *[fixed in phase 0]*
`cuenv_version: env!("CARGO_PKG_VERSION")` invalidates the entire cache on
every release. Tolerable locally; catastrophic once a shared remote cache
exists. Needs an explicit `action_semantics_version` integer, bumped only
when execution semantics change.

**F7 — Action-result integrity is unchecked.** *[fixed in phase 0]*
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
**F13** *[fixed in phase 0]* — `normalize_workdir` fell back to an absolute
path when the workdir was under neither the project nor the module root,
baking a machine-specific path into the key.
**F14** — Symlinks are silently dropped from input trees
(`merkle.rs:72`); `SymlinkNode` is modelled but always empty.
**F15** *[fixed in phase 0]* — Neither the `Action` nor the `Command` blob was
stored, so a cache miss could not be explained or diffed against the
previous run.
**F16** — Cached stdout/stderr replay as two blocks, losing interleaving.
**F17** — `--show-cache-path` and `--materialize-outputs` are parsed, plumbed
into `ExecutorConfig` (`executor.rs:88`, `executor.rs:92`) and never read. The
docs told users to use them. Documented as unimplemented in phase 0; the flags
become real with the `cuenv cache` surface in phase 2.

**F19 — Output collection walked the entire working directory.** *[fixed]*
`collect_outputs` built a globset and then walked all of `workdir` to filter
against it, so a task declaring `target/release/app` traversed `node_modules`
and the whole of `target` on every recorded run — a full tree walk to avoid
work, which is the cost a cache exists to remove. Walk roots are now derived
from each pattern's literal prefix, collapsed where they nest.

**F20 — No way to bypass the cache for one run.** *[fixed]* The only way to
bust a bad entry was editing CUE. `CUENV_CACHE=off|read|write|read-write` now
overrides every task's mode for a single invocation, matching what moon's
`MOON_CACHE` is for. It can only narrow a task's declared policy, never widen
it, so it cannot start caching a task that opted out.

### Found during review, fixed immediately

**F18 — Resolved secrets were written into the CAS in plaintext.** *[fixed]*
`apply_task_environment` resolves project-level secrets and `set`s them into
`Environment.vars`, a plain `HashMap<String, String>` with no marking. That map
went verbatim into `Command.environment_variables`, and phase 0's own F15 fix —
storing the `Command` blob so a miss is explicable — turned what had been a
one-way hash into **plaintext credentials at rest**. The REAPI transport would
then have uploaded them to a shared server on the first `read-write` run.

Ironically the code already refused to cache *task-level* env
(`CacheSkipReason::RuntimeEnv`) on exactly these grounds, one line after
folding project-level secrets straight in.

Fixed by §6.6 below. The lesson worth keeping: storing a blob is not the same
risk as hashing it, and F15 changed which one the cache was doing.

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

### Phase 0 — Stop lying (correctness, no new capability) — **done**

- **Declared env only. (F3)** `hermetic` now accepts an options form,
  `{passthrough: [...]}`, naming the host variables an action may depend on.
  `Environment::action_environment` records the declared CUE environment plus
  those names; `merge_with_system_hermetic` is off the key path entirely.
- **`action_semantics_version: u32` replaces `cuenv_version`. (F6)** Defined
  as `cuenv_cas::ACTION_SEMANTICS_VERSION` with a documented bump rule, so a
  release no longer invalidates the world.
- **Dangling entries degrade to misses. (F7)** `cuenv_cas::missing_blobs`
  walks everything an `ActionResult` references — output files, stdout,
  stderr, and output directory trees transitively — before a hit is served.
  Materialization stages into a scratch directory inside the workspace and
  commits by rename, so a missing or corrupt blob cannot leave a tree that is
  half cached output and half whatever was there before. Cached output paths
  that would escape the working directory are rejected outright.
- **`Action` and `Command` blobs are stored. (F15)** Both are written to the
  CAS at key-computation time, which is what phase 4's `explain` needs to
  diff two keys rather than just report that they differ.
- **Unportable working directories refuse to cache. (F13)** A workdir under
  neither the project nor the module root skips with
  `CacheSkipReason::UnportableWorkdir` instead of baking `/home/<user>/…`
  into the key.
- **`hermetic` means something. (F1, partial)** `hermetic: false` now skips
  the cache (`CacheSkipReason::NonHermetic`) rather than recording an entry
  keyed on a fraction of what produced it. The schema, the ADR and the
  coverage matrix all state plainly that filesystem isolation is not yet
  implemented.

**Deliberately deferred to phase 1:** execution environment is unchanged. A
task still *receives* ambient `HOME`, `TERM`, `XDG_*` and friends even though
its key no longer records them. Making the key match reality requires the
exec root and sandbox below; until then the narrow exposure — a cached task
whose result depends on an undeclared ambient variable — is strictly smaller
than F1, which lets it depend on undeclared *files*. Cache mode defaults to
`never`, so this reaches only tasks that explicitly opted in.

*Exit (met):* two machines with identical checkouts and toolchains compute
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
- Make `--show-cache-path` and `--materialize-outputs` do what they say, or
  remove them. **(F17)**

*Exit:* warm no-op run on a large monorepo is dominated by process spawn,
not by hashing or copying; the cache respects a configured size budget.

### Phase 3 — Remote cache

- **Done: canonical encoding is REAPI protobuf.** `cuenv_cas::reapi` converts
  cuenv's messages to `build.bazel.remote.execution.v2` types and digests
  their protobuf bytes; the local CAS and action cache now store exactly what
  a REAPI server exchanges. The semantics version travels in REAPI's
  `Action.salt`, and `ACTION_SEMANTICS_VERSION` went to `2`, invalidating
  every pre-existing entry as intended. Bindings come from
  `bazel-remote-apis`, which ships pre-generated prost/tonic code, so no
  `protoc` is needed at build time and the Nix build is untouched.
- **Done: the store traits are async.** `Cas` and `ActionCache` are
  `#[async_trait]`, so a store can be remote. They were synchronous, and
  gRPC is not; the alternative was blocking an executor worker thread on
  network I/O and serializing the task graph behind it.
- **Done: `cuenv-cas-remote`.** A REAPI client plus `LayeredCas` /
  `LayeredActionCache`, which read through a local store to a remote one and
  keep what they fetch. A remote failure degrades — an unreachable cache is
  a miss, a failed upload is a warning — because a cache is an optimization
  and a dead network should make a build slower, not broken.
- **Done: the gRPC client** (tonic + `bazel-remote-apis`) covering
  `ContentAddressableStorage`, `ActionCache`, `ByteStream` and
  `Capabilities`. It honours `max_batch_total_size_bytes` — batching small
  blobs, streaming large ones — skips uploading blobs `FindMissingBlobs`
  says the server already holds, refuses a server that does not offer
  SHA-256 rather than missing forever, and verifies every fetched blob's
  digest before the bytes reach the workspace.
- **Done: auth.** Bearer tokens and arbitrary headers, with credentials
  redacted from every `Debug` and error message, and non-printable
  credential bytes rejected up front rather than becoming an opaque 401.
- **Still to do: wiring.** `cuenv task` does not build a remote store yet.
  That needs the `#Cache.remote` schema below, CLI plumbing, and the
  decision about when writes are allowed (see §6.5).
- HTTP/object-store fallback (bazel-remote HTTP layout over S3/GCS/R2) for
  teams without a gRPC endpoint.
- Auth: bearer headers and mTLS; read-only credentials for untrusted PR
  builds so a fork cannot poison the shared cache.
- Only tasks that ran at tier `strict` or `sandbox-exec` write to the
  remote cache by default. **(F1, enforced)** Until phase 1 lands there is
  no such tier, which is why `RemoteConfig` is read-only unless
  `writable()` is called.

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

### 6.7 Consuming another task's output

Bazel and buck2 both treat a reference to another target's output as two
things at once: a dependency edge, and a set of input files. cuenv treated it
as neither — `Input::Task` created no edge (`project.rs` added one only for
`Input::Project`, and no graph code reads `inputs` at all), and the cache
refused the task outright with `NonPathRef`.

Both are now derived from the reference. `inputs: [{task: "build"}]` adds the
implicit `dependsOn` and expands to `build`'s declared `outputs` as ordinary
path inputs, hashed from disk like any other.

**Hashing the produced content, not the producer's key, is the important
part.** Folding the producer's action digest into the consumer — which is what
moon does — means any change to the producer's inputs invalidates every
consumer, even when the producer rebuilt byte-identical output. Hashing the
output gives *early cutoff*: touch a comment in `build.rs`, `build` reruns,
its output is unchanged, and every downstream `test` still hits. This is the
property that makes a monorepo cache worth having, and it is why Bazel and
buck2 both work this way.

A consequence worth stating: `dependsOn` on its own contributes nothing to the
key, which is also Bazel's semantics — an edge that provides no files cannot
change your output. With filesystem isolation (F1) open, a task *can* read a
dependency's output without declaring it, and would then get a stale hit.
Declaring the output as an input is both the fix and the thing that buys
early cutoff, so it is what the docs tell users to do.

**Cross-project references are not done.** `#ProjectReference` still skips
with `NonPathRef`: the producer's outputs live under a sibling project root,
while the input hasher is rooted at the consuming project
(`commands/task/mod.rs`), so `prefix_patterns_for_hasher_root` cannot express
them. Fixing that means rooting the hasher at the CUE module root and
rebasing per task. Also note `Mapping.to` is a materialization destination
that nothing currently creates; only `from` is hashed.

### 6.6 How secrets enter a cache key

A cache key has to change when a secret changes, or a rotated credential keeps
serving results produced with the old one — and from a *shared* cache, that is
a "deploy succeeded" entry served after the deploy key was revoked. The key
must also never carry the value, because it is stored in the CAS and a remote
cache ships it off the machine.

Two representations were considered:

1. **The secret's reference** (`op://Engineering/prod-db/password`). Rejected:
   rotation does not change it, so it produces exactly the stale hit above; the
   reference itself leaks vault structure to the server; and `exec`-derived
   secrets have no reference at all, only a command line.
2. **A salted keyed hash of the value.** Adopted. Rotation invalidates, the
   value is not recoverable without the salt, and `exec` is covered because the
   fingerprint is taken over the resolved output rather than the command.

`cuenv-secrets` already had this for the CI pipeline, so the action key reuses
it: `cuenv-secret-fp:<hmac-sha256(salt, name ‖ value)>`, keyed by
`CUENV_SECRET_SALT`. The construction was documented as HMAC but implemented as
`H(salt ‖ name ‖ value)`, which is length-extendable; it is now a real HMAC with
the name length-prefixed so `("AB","C")` and `("A","BC")` cannot collide.

**With no salt configured, the task is not cached** —
`CacheSkipReason::SecretsWithoutCacheSalt`. Including the value is unsafe and
omitting it would let two different credentials key identically, so there is no
third option. This is a deliberate refusal rather than a silent degradation.

Residual risk, stated plainly: a low-entropy secret is brute-forceable by
someone who holds both the store and the salt, and a secret baked into a task's
*output* is not keyable at all. Both are the user's to manage, as with moon.

### 6.5 When may cuenv write to a shared cache?

A shared cache multiplies the consequence of an unsound entry: a wrong result
stops being one developer's confusing afternoon and becomes every machine's.
F1 is still open — a task can read a file it never declared — so an entry
cuenv writes today may be wrong on another machine.

The client therefore defaults to read-only and will not upload unless
`RemoteConfig::writable()` is called. When the schema lands, `#Cache.remote.mode`
should default to `"read"` for the same reason, and the recommendation should
stay "read-only until phase 1" until filesystem isolation exists.

Reading from a shared cache is not risk-free either — you consume whatever
someone else produced — but the exposure is bounded by the key, which now
records the declared inputs, the declared environment, the platform and the
runtime identity. Writing is what turns one machine's unsound entry into
everyone's.

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
