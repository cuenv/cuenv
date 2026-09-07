---
id: ADR-0010
title: Direct rio-vt Integration for Cuetty
status: Accepted
decision_date: 2026-09-07
approvers:
  - Core Maintainers
related_features:
  - Cuetty
supersedes: []
superseded_by: []
---

## Context

Cuetty's proof of concept currently hosts the pinned Rio revision
`b0b79c1ebadc8d6a9a79c4c44a91a42b3ea439d1` through `librio::Surface` and a
renderer-neutral `TerminalSession` boundary. That made a small live terminal
possible quickly, but the original render snapshot exposed only one codepoint
per cell and did not surface enough terminal-mode state for a daily-driver
host.

Current upstream `librio` has improved substantially. It now exposes grapheme
attachments, cursor shape and visibility, mode-aware paste, history search,
and mouse routing. The issue is not that Rio lacks terminal capability.

It is that `librio` is deliberately Rio's C ABI. Upstream directs Rust
embedders to the safe `rio-vt` crate and identifies `librio` as the reference
implementation for PTY, grid-snapshot, and renderer integration. Cuetty is a
Rust GPUI application, so it should not make a C-oriented wrapper its
long-term terminal abstraction.

`rio-vt` exposes public state and events that the wrapper still hides from a
host:

- full terminal modes, including focus reporting and application keypad;
- complete keyboard-mode flags;
- child-exit status and PTY ownership;
- clipboard load, colour-query, and text-area-size requests;
- history, eviction count, selection, search, and viewport controls;
- cell text, styles, grapheme data, hyperlinks, cursor state, and terminal
  events under the same safe-Rust API.

These are not optional details for safe close behaviour, full-screen TUIs,
correct paste, or a host that must choose an explicit clipboard and query
policy.

Primary upstream references:

- [Rio's embedding announcement](https://rioterm.com/blog/2026/07/27/rio-vt-and-librio)
- [`rio-vt` README at the qualified upstream revision](https://github.com/raphamorim/rio/blob/7ae087500bcde5c0c9f09cb9c50382e9220b3360/rio-vt/README.md)
- [`rio-vt` PTY machine](https://github.com/raphamorim/rio/blob/7ae087500bcde5c0c9f09cb9c50382e9220b3360/rio-vt/src/performer/mod.rs)
- [`rio-vt` terminal state](https://github.com/raphamorim/rio/blob/7ae087500bcde5c0c9f09cb9c50382e9220b3360/rio-vt/src/crosswords/mod.rs)
- [`librio`'s current Rust-embedder guidance](https://github.com/raphamorim/rio/blob/7ae087500bcde5c0c9f09cb9c50382e9220b3360/librio/README.md)

## Decision

Cuetty will keep Rio's terminal core but replace its `librio::Surface`
implementation with a direct, pinned `rio-vt` implementation behind
`TerminalSession`.

The migration has these boundaries:

- Keep GPUI, the workspace layout model, the event bridge, and Cuetty's
  renderer-neutral `TerminalFrame` architecture.
- Build a Cuetty-owned session from public `rio-vt` APIs: `Crosswords`,
  `FairMutex`, `Machine`, `teletypewriter`, and an `EventListener` that only
  enqueues host effects for the UI thread.
- Keep Rio types inside the adapter. GPUI, workspace, and configuration code
  must not depend on `rio-vt` types directly.
- Replace the lossy `TerminalCell::codepoint` model with text clusters plus
  engine-declared width, continuation, style, hyperlink, and wrap semantics.
- Let the terminal engine remain the source of truth for history, selection,
  search, modes, and protocol replies. Cuetty must not recreate a second ANSI
  parser, terminal-mode tracker, or scrollback store.
- Implement the host input encoder as a tested Cuetty module. Rio's
  `librio/src/key.rs` is a useful MIT-licensed reference, but copied code must
  preserve notices and unsupported keyboard capabilities must not be
  advertised. Prefer a small upstream extraction where practical.
- Start with a conservative `TERM` and terminfo policy. Do not advertise Kitty
  keyboard or graphics support until the exact protocol surface is tested.

The qualification baseline is upstream revision
`7ae087500bcde5c0c9f09cb9c50382e9220b3360`, not an unbounded branch name.
The implementation may move to a tested release tag after verifying that the
required public API is present. It must pin the chosen source revision and
record the Rust toolchain used to build it.

## Alternatives considered

### Upgrade the `librio` pin only

Rejected as the long-term design. It would resolve several original blockers
with the smallest code delta, but still hides focus reporting, application
keypad state, child-exit status, clipboard-load policy, and host-resolved
colour and geometry queries. Repairing that downstream requires an upstream
wait or a wrapper fork. It also conflicts with upstream's stated split between
safe Rust (`rio-vt`) and C ABI (`librio`).

### Keep the existing pin

Rejected. Its public snapshot is intentionally too lossy for complete
grapheme, input-mode, and history behaviour. Adding workarounds in Cuetty would
duplicate terminal state and produce fragile full-screen and resize semantics.

### Replace Rio with another terminal engine

Rejected for now. `rio-vt` supplies the required public integration points,
has an MIT licence compatible with Cuetty, and lets the app preserve its
current architecture. An engine replacement is justified only if the
qualification spike identifies an unfixable Rio-core or public-API failure.
GPUI input, IME, or renderer defects alone are not grounds to change engine.

## Consequences

### Positive

- Cuetty can own its lifecycle and protocol decisions instead of losing events
  inside a C-oriented bridge.
- The data model can become grapheme-aware without private Rio access.
- History, selection, search, cursor, and input modes have one authoritative
  terminal-state owner.
- The implementation follows Rio's supported Rust embedding path while keeping
  the existing replaceable session seam.

### Negative

- This is a bounded host integration project, not a dependency-version bump.
  Cuetty must own PTY setup, event dispatch, key encoding, shutdown, and
  snapshot conversion.
- Rio's current workspace declares Rust `1.96.1`. Cuetty's app flake and the
  root flake now select Rust `1.98.1`, with updated `rust-overlay` locks.
  This resolves the old `1.90.0` version mismatch, but build and runtime
  compatibility remain qualification gates before the migration is accepted.
- `rio-vt` is pre-1.0 and evolves quickly. Pinning, conformance tests, and
  explicit upgrade review are required.
- Current Rio input support is not complete by itself. In particular, do not
  claim complete Kitty keyboard support until Cuetty's encoder proves the
  specific modes it implements.

### Neutral

- This decision does not commit Cuetty to Kitty graphics, remote sessions,
  plugins, Windows, or Linux support.
- `rio-vt` and its root repository are MIT licensed; Cuetty remains AGPL. Any
  copied source retains its upstream notices, and graphics/input provenance is
  audited separately before copying.

## Qualification and validation

Run a ten-working-day spike before changing the main session implementation.
It is restricted to one GPUI pane and excludes tabs, splits, settings, Cuenv
UX, and broad renderer work.

1. **Public API and build proof.** Construct a `rio-vt` session using only
   public PTY APIs; prove input, resize, final-output drain, child status, and
   bounded teardown. Record the macOS Rust toolchain and no-private-access
   proof.
2. **Lossless frame and history proof.** Snapshot full cell text, styles,
   wide placeholders, wraps, links, cursor visibility/shape, native history,
   selection, and search. Test combining accents, emoji ZWJ sequences, CJK,
   last-column wide glyphs, alternate screens, resize/reflow, and split input
   chunks.
3. **Input and host protocol proof.** Assert actual PTY bytes for legacy and
   application keys, application keypad, bracketed paste, mouse, wheel,
   focus, title, bell, OSC 7, OSC 8, OSC 52 policy, and host-resolved query
   replies.
4. **macOS lifecycle proof.** Exercise the default login shell, an editor,
   `ssh`, `fzf`, `tmux` when used, and relevant Kubernetes TUIs. Verify dead
   keys, one IME, Option policy, foreground and background jobs, and repeated
   spawn/close cycles.
5. **Decision gate.** Measure input latency, resize, render cost, and memory
   for 10,000 and 100,000 history lines on the target Mac. Compare the same
   workflow qualitatively with Ghostty. Merge the migration only if all
   must-have paths pass without terminal-core defects.

The spike fails only when a must-have behaviour is absent or incorrect in
Rio's public core or bounded integration surface. It does not fail merely
because a Cuetty host or renderer feature remains to be implemented.
