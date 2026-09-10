---
title: Cuetty M0 Capability Evidence
description: Machine-readable qualification status for the direct rio-vt integration.
---

The checked-in `apps/cuetty/m0/capabilities.json` records the exact Rio revision,
Rust toolchain, retained control revision, qualification requirements, source
fixtures, implementation gaps, and executed evidence. It is a qualification
ledger, not a generated claim that source code passes terminal conformance.

The report retains the original Linux qualification failures against
`7ae087500bcde5c0c9f09cb9c50382e9220b3360` as historical executions and records
new evidence against `b0694c0707a90dc93fdf01cbf8a658424be285ee` separately.
The current revision is the head of
[Rio PR #1927](https://github.com/raphamorim/rio/pull/1927). Every capability
has a decision outcome of `pass`, `fail`, or `unobservable`; a pass must cite a
successful execution against the current engine revision.

Recorded Linux evidence includes 59 unit tests and four public-core contract
tests passing in a harness that imports the actual terminal modules but excludes
GPUI and cuengine. Four real-PTY tests were ignored in that ordinary run.
Explicit PTY execution failed: parallel execution passed two and failed two;
serial execution passed three and failed paste. Three initial full-app test
attempts stopped at cuengine's missing-Go build prerequisite, not a terminal
assertion. After a task-scoped Go installation, cuengine and Cuetty compiled,
but test linking failed on missing Linux xcb/xkbcommon/xkbcommon-x11 libraries.
No full-app tests ran in that retry. These results do not establish a macOS app
build or smoke pass.

Those Linux results remain useful history, but they do not describe the new
pin. On Apple Silicon macOS, the full app suite, serial real-PTY suite, and the
formerly failing contention regression pass. The PTY suite covers input,
bracketed paste, resize, final output plus exit status, Rio-owned history
navigation, and bounded child reaping. Cargo still reports transitive
future-incompatibility notices for `block` and `proc-macro-error2`; neither is a
Cuetty Clippy diagnostic.

On the original revision, repeated sessions and a controlled terminal-lock contention probe observed a
child exiting successfully while its final output was missing from the frame.
Bracketed mode and encoded bytes were correct. The historical report marked
final-output delivery and the paste round trip `fail`, with root-cause attribution qualified:
Rio's `Machine::pty_read` appears able to discard buffered bytes if the terminal
lock is unavailable and a subsequent Linux read returns EIO. This is a
source-backed inference, not an instrumented execution trace. The repository's
`host.rs::final_output_survives_snapshot_lock_contention` regression now passes
in the ordinary suite against PR #1927 after failing through the earlier
actual-module harness with the old pin.
The sanitized command/output evidence lives at
`apps/cuetty/m0/evidence/2026-09-07-linux-validation.txt`.
The current Apple Silicon evidence is recorded at
`apps/cuetty/m0/evidence/2026-09-10-macos-rio-pr-1927.txt`.

The original Rio `teletypewriter` revision had a source-inspected lifecycle blocker:
`next_child_event` reaps the child, but `Child::drop` later sends SIGHUP to the
stored PID unconditionally, risking a signal to a recycled PID. Shutdown also
lacks a guaranteed reap. PR #1927 introduces an ownership-safe, idempotent
shutdown/reap path. Cuetty's `real_session_close_eventually_reaps_the_child`
test observes the app's asynchronous close path reaching a reaped child within
its bound; intentionally leaking the PTY is still not an acceptance strategy.

From the repository root, check the JSON structure, required capability inventory,
evidence paths/symbols, exact revision, and consistency of promotion claims:

```sh
node apps/cuetty/m0/validate-report.mjs
```

This command does not compile Cuetty, execute the Rust fixtures, or qualify the
terminal. Focused app-local Nix tests and explicit real-PTY tests must be run in
the configured development environment. Tests marked `#[ignore]` require an
explicit invocation; an ordinary green test run is not PTY evidence. The Apple
Silicon workload and performance corpus need the target Mac. The root Nix flake
check remains the review/merge gate for the production dependency migration.

When a check runs, add an `executions` record with `id`, `command`, `platform`,
`rust_toolchain`, `engine_revision`, `executed_at`, `exit_code`, and a repository-
relative `log_path` to a captured, nonempty log. Link its ID from only the exact
capability scopes that it exercised. Do not store secrets, terminal scrollback
from real accounts, or environment dumps in evidence logs. Set `verified`/`pass`
only after inspecting those results; a zero exit status alone is not proof that
an ignored test ran. Failed executions use `fail` with an attributed `gap_owner`.

Promotion requires all M0-required scopes to be verified, including the retained
`librio` comparison and real Apple Silicon corpus. Current host gaps include
colour queries, focus/mouse routing, keypad identity, hyperlink destinations,
selection/search across history, and IME. Extended keyboard and Kitty graphics remain
explicitly unqualified and cannot be advertised as implemented. Unsupported
Cuetty host or renderer work alone does not disqualify Rio.

See the [daily-driver roadmap](/explanation/cuetty-daily-driver-roadmap/) and
[ADR-0010](/decisions/adrs/adr-0010-cuetty-direct-rio-vt-integration/) for the goal,
scope, and engine decision.
