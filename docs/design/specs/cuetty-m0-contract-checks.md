# Cuetty M0 contract checks

M0 qualifies the public `rio-vt` core and Cuetty's direct session boundary.
A passing parser test alone does not qualify the renderer, PTY lifecycle, or
the application as a daily driver. See
[ADR-0010](../../src/content/docs/decisions/adrs/adr-0010-cuetty-direct-rio-vt-integration.md).

## Executable fixtures

`apps/cuetty/tests/rio_vt_contract.rs` exercises public core APIs without a
PTY: output after resize, application-cursor and bracketed-paste mode changes,
combining marks with wide-cell spacers, and cursor/alternate-screen state.
These are upstream capability probes, not tests of Cuetty's adapter.

`apps/cuetty/tests/fixtures/m0-pty-child.sh` is a deterministic child for
session tests. Spawn `/bin/sh` with the absolute fixture path and arguments
below. It has no dependency on a user's shell configuration. Except for `exit`,
wait until the sampled frame contains `M0-READY` before sending input.

| Scenario | Child arguments | Host action and assertion |
| --- | --- | --- |
| Spawn, final output, exit | `exit` | Observe `M0-FINAL-OUTPUT` before discarding the session; child status is 23. A close notification must occur exactly once. |
| Resize | `resize` | Resize to 50 columns by 10 rows, then send `size` plus Enter; require frame dimensions 50 by 10 and `M0-SIZE:10 50`. |
| Raw input | `raw 3` | Send `a`, Control-C, Enter; require `M0-BYTES:61030d`. Raw mode means Control-C must be input, not a signal. |
| Plain paste | `raw 3` | Paste `abc`; require `M0-BYTES:616263`, without bracket markers. |
| Bracketed paste | `bracketed 15` | Paste `abc`; require `M0-BYTES:1b5b3230307e6162631b5b3230317e`. |
| Close | `hold` | Close twice; both calls are safe. New input, paste, and resize return closed-session errors. Confirm the reader and child terminate, not merely that an option was cleared. |

Use a bounded condition wait for each asynchronous assertion, not a fixed sleep.
On timeout, include the most recent frame and received control events. Each
test must close its session on success and failure. Report skipped PTY tests
as skipped, never passed. The fixture's exit and byte probes require no
interactive shell; `stty`, `dd`, `od`, and `tr` must be in the child's PATH.

The production session's `real_session_*` unit tests are explicitly ignored
by the ordinary test suite. Run them with
`cargo test real_session -- --ignored` in the app-local development environment.
They cover final output and raw exit status, post-exit operation rejection,
actual key ingress, plain/bracketed paste, and grid/child resize agreement.
The current suite also includes a bounded close/hold reap assertion and an
authoritative-history navigation fixture.

## Lifecycle and final-output qualification

At the original ADR qualification revision
`7ae087500bcde5c0c9f09cb9c50382e9220b3360`, source inspection found a Rio-core
lifecycle defect: [`Pty::next_child_event`](https://github.com/raphamorim/rio/blob/7ae087500bcde5c0c9f09cb9c50382e9220b3360/teletypewriter/src/unix/mod.rs)
reaps an exited child with `waitpid`, while the same file's `Child::drop`
unconditionally sends `SIGHUP` to its saved numeric PID later. If that PID has
been recycled before destruction, the signal could target a different process.
This was a source-level race finding, not an observed recycled-PID incident.

Additionally, shutting down the reader before natural child exit leaves no
reader event loop to reap the child after `Child::drop` sends `SIGHUP`.
[`Machine`](https://github.com/raphamorim/rio/blob/7ae087500bcde5c0c9f09cb9c50382e9220b3360/rio-vt/src/performer/mod.rs)
keeps its PTY private and exposes no public disarm or owned-child teardown
operation. A workaround that intentionally leaked child allocations was
rejected.

Cuetty now pins `b0694c0707a90dc93fdf01cbf8a658424be285ee` from
[Rio PR #1927](https://github.com/raphamorim/rio/pull/1927). That revision gives
the PTY owner an idempotent shutdown operation, retires reaped PIDs before any
signal, escalates from SIGHUP after a bounded grace period, and waits to reap.
Cuetty requests shutdown and joins the returned machine off the GPUI thread.
The `real_session_close_eventually_reaps_the_child` fixture observes that the
owned child disappears within its five-second test bound.

### Final-output delivery regression

Supplemental Linux PTY validation on the original revision also observed a child exit successfully
while its final output was absent from the terminal grid. The controlled
reproducer uses a first session, so prior-session cleanup or PID recycling
is not necessary to trigger it:

1. Spawn the `raw 3` fixture and wait for `M0-READY`.
2. Hold the terminal's `FairMutex`, send `abc` on the production Machine
   channel, and hold that lock for 100 ms while the child writes and exits.
3. Release the lock and observe `ChildExited` with exit code 0 and `Close`.
4. Require the final grid to contain `M0-BYTES:616263`.

The expected text was missing in that controlled validation run. Inspection of
[`Machine::pty_read`](https://github.com/raphamorim/rio/blob/7ae087500bcde5c0c9f09cb9c50382e9220b3360/rio-vt/src/performer/mod.rs)
shows a compatible failure mechanism: bytes buffered while the terminal lock
is unavailable can be discarded if the next read returns an error such as
Linux PTY `EIO`, before those buffered bytes reach the parser. The missing
output is observed; that source-level mechanism is the current diagnosis,
not a PID-reuse claim.

The ordinary host test
`final_output_survives_snapshot_lock_contention` preserves this regression.
It passes at the pinned PR revision on Apple Silicon macOS and remains in the
default suite so a future dependency update cannot silently reintroduce the
loss. It does not use `should_panic` or count reproduction as a pass.
All host PTY tests share a test-only serialization guard to avoid unrelated
parallel process-notification interference. Final-output delivery remains an
M0 blocker, and passing pure encoders or an isolated paste probe does not
qualify the corresponding end-to-end PTY behavior.

### Repair boundary and next engineering action

The following API-boundary review is against the exact qualification revision
above. It is source inspection, not a new executed regression result.

| Boundary | Source evidence | Consequence for Cuetty |
| --- | --- | --- |
| Child ownership | `teletypewriter/src/unix/mod.rs`: `Child::waitpid(&self)` (line 875) reaps without recording completion; `Child::drop` (line 905) always signals the stored PID. Its `process` field is private. | Joining the reader, checking a PID, or waiting longer does not disarm destruction. Another wait after reaping cannot establish ownership of a recycled PID. |
| Machine ownership | `rio-vt/src/performer/mod.rs`: `Machine.pty` (line 108) and `State.parser` are private. The public machine operations are `new`, `channel`, and `spawn`. | The joined `(Machine, State)` does not expose an owned PTY teardown or a way to recover and parse discarded bytes. `Msg::Shutdown` exits the event loop, not a child-termination-and-reap protocol. |
| Buffered output | `Machine::pty_read` keeps `unprocessed` locally (line 224), reads again when the terminal lock is unavailable, and returns a non-retryable read error (line 245) before `parser.advance` (line 263). | Bytes already consumed from the kernel can be lost on EIO; a later frame sample or final read cannot recover them. Shorter snapshot locks reduce probability, not the failure condition. |
| Exit drain | The child-exit branch calls `pty_read` once, then emits `ChildExited` and leaves the event loop. A read call can stop at `MAX_LOCKED_READ`; Unix HUP readiness is skipped in the ordinary I/O branch. | A repair must distinguish a fairness yield from a fully drained PTY and cover residual output at EOF/HUP, not only the observed small-marker case. This is an additional source-review requirement, not a separately reproduced failure. |

There is no small, ownership-safe host workaround while retaining the pinned
`Machine<Pty, Listener>`. A custom public `EventedPty` reader could translate
Linux EIO to EOF to avoid that particular early return, but it would leave the
upstream child destructor and complete-drain contract unresolved. Mutating the
public numeric PID to a sentinel, duplicating the master descriptor, or leaking
the child is not an ownership fix. Replacing PTY spawning, child ownership, and
the event loop with a separately qualified implementation is technically
possible through public core APIs, but is a new backend project rather than a
safe local repair of this integration.

The preferred next action is a focused upstream patch (or an explicitly
approved, pinned fork) for both layers:

1. Give the child a single synchronized ownership state. Record successful
   reaping before any destructor can signal it, preserve its exit status, and
   make termination plus reaping explicit and idempotent. Run potentially
   blocking cleanup off the UI thread. Specify behavior for a child that
   ignores SIGHUP and for errors during machine setup or reader execution.
   A prior numeric-PID existence check is insufficient because it introduces
   another check/use race.
2. Preserve or parse every buffered byte before handling EOF or a terminal
   read error. Keep bounded parsing for ordinary fairness, but make final
   draining continue through those yields until the defined completion
   condition. Publish exit only after final buffered output and pending
   synchronized-update state are committed. Define a bounded policy when a
   descendant keeps the slave open rather than blocking indefinitely.
3. Add a deterministic synthetic `EventedPty` test that handshakes with the
   snapshot lock: return marker bytes, then EIO, and assert the marker is
   committed before exit. Also cover zero-byte EOF, HUP with residual output,
   output exceeding one parsing budget, and pending synchronized updates.
   The synthetic transport should own no real process, isolating parser-drain
   correctness from the unsafe lifecycle. Test real natural exit, close/hold,
   repeated close, reap completion, and no signaling after reaping separately.
4. Repin only after reviewing and validating the ownership fix. Explicitly run
   the existing ignored host regressions on Linux and macOS, capture the new
   evidence, and update capability outcomes only for the scopes those tests
   establish. Production dependency changes also require the root flake gate.

No lifecycle race was executed during this API review, and no production
workaround or engine-pin change is implied by these repair requirements.

## Remaining host protocol gaps

The session uses live legacy application-cursor and bracketed-paste modes.
It rejects extended keyboard input explicitly when Kitty keyboard flags or
`modifyOtherKeys` are active; key releases, IME/committed text, keypad identity,
focus reporting, and mouse reporting remain unwired. Color queries currently
receive no reply because the session has no exact configured renderer palette;
text-area size queries do receive a reply. OSC 52 clipboard reads return an
empty response by policy rather than exposing the system clipboard.

## Adapter and lifecycle regression requirements

- Feed `e` plus U+0301, a wide CJK scalar, and a mode-2027 ZWJ emoji through
  the parser. The sampled frame and copy text must preserve every codepoint
  exactly once, with no text on leading/trailing spacer cells.
- Sample cursor shape, visibility, grid dimensions, and rows under one lock.
  Repeat sampling after resize and alternate-screen switching; every frame
  must pass `TerminalFrame::validate`.
- Final child output must remain sampleable when the child exits immediately.
  Retain exit status separately from the exactly-once close notification.
- Close a waiting child, then close again. Verify reader completion and child
  cleanup with bounded waits; sending shutdown alone is not proof of cleanup.
- Report defects by layer: Rio public/core limitation, Cuetty host/adapter,
  Cuetty rendering/input, or environment. Host gaps do not disqualify Rio.

## Validation gate

Run the focused app-local Nix tests/clippy checks and the
`rio_vt_contract` integration target in the same Nix development environment.
Production dependency and Nix changes additionally require the root
`nix flake check -L --accept-flake-config` gate. Run
`cuenv task ci.schema-docs-check` for these docs. An unavailable compiler,
Nix installation, or macOS test host is a validation blocker, not a passing
result. Apple Silicon real-workload smoke coverage remains separately required
by the daily-driver roadmap.
