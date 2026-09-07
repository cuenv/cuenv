---
title: Cuetty M0 Capability Evidence
description: Machine-readable qualification status for the direct rio-vt integration.
---

The checked-in `apps/cuetty/m0/capabilities.json` records the exact Rio revision,
Rust toolchain, retained control revision, qualification requirements, source
fixtures, implementation gaps, and executed evidence. It is a qualification
ledger, not a generated claim that source code passes terminal conformance.

The report contains **no fully verified M0-required capabilities**. Supplemental
Linux tests passed for portions of the implementation, recorded as
`implemented_partially_validated`; source-only implementations remain
`implemented_unvalidated`. Current Cuetty host limitations are `unsupported`;
unattempted work is `deferred`. Every capability also has a decision outcome:
`pass`, `fail`, or `unobservable`. Partial evidence cannot promote a broader
requirement to `pass`.

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

The subsequent full-app `cargo check --locked`, all-target Clippy with
`-D warnings`, and formatting check passed. This includes Rust typechecking of
GPUI and cuengine, but not native linking or full-app test execution. Cargo
reported an existing transitive `proc-macro-error2` future-incompatibility
notice; it was not a Cuetty Clippy failure. Nix and macOS gates remain unrun.

Repeated sessions and a controlled terminal-lock contention probe observed a
child exiting successfully while its final output was missing from the frame.
Bracketed mode and encoded bytes were correct. The report marks final-output
delivery and the paste round trip `fail`, with root-cause attribution qualified:
Rio's `Machine::pty_read` appears able to discard buffered bytes if the terminal
lock is unavailable and a subsequent Linux read returns EIO. This is a
source-backed inference, not an instrumented execution trace. The repository's
ignored regression is
`host.rs::known_upstream_regression_final_output_survives_snapshot_lock_contention`.
It also failed through the actual-module harness after confirming child exit
code zero, reproducing the missing final marker with repository-owned assertions.
The sanitized command/output evidence lives at
`apps/cuetty/m0/evidence/2026-09-07-linux-validation.txt`.

The pinned Rio `teletypewriter` has a source-inspected lifecycle blocker:
`next_child_event` reaps the child, but `Child::drop` later sends SIGHUP to the
stored PID unconditionally, risking a signal to a recycled PID. Shutdown also
lacks a guaranteed reap. The report marks this `blocked_by_rio_core`, attributed
specifically to the PTY ownership dependency, not the VT parser. No executable
race reproduction has run, so its outcome remains `unobservable`. A safe
ownership fix is required; intentionally leaking the PTY is not an acceptance
strategy. The report links the exact upstream source revision and symbols.

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
native history interaction, and IME. Extended keyboard and Kitty graphics remain
explicitly unqualified and cannot be advertised as implemented. Unsupported
Cuetty host or renderer work alone does not disqualify Rio.

See the [daily-driver roadmap](/explanation/cuetty-daily-driver-roadmap/) and
[ADR-0010](/decisions/adrs/adr-0010-cuetty-direct-rio-vt-integration/) for the goal,
scope, and engine decision.
