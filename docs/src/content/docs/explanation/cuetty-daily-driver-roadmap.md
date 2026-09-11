---
title: Cuetty daily-driver roadmap
description: A focused macOS roadmap for making Cuetty a dependable replacement for a vanilla Ghostty workflow.
---

# Cuetty daily-driver roadmap

This is a deliberately narrow plan for a macOS developer moving from a mostly
vanilla Ghostty setup. The goal is not Ghostty feature parity. The goal is that
Cuetty can be used for normal engineering work without keeping Ghostty open as
the reliable terminal.

The product sequence matters:

1. Make one terminal session correct and compatible.
2. Make normal terminal work comfortable and persistent.
3. Finish the small macOS shell that makes tabs and splits safe.
4. Package, test, and run it as a canary before it becomes the primary app.
5. Only then add Cuenv-specific advantages.

That sequence prevents the project from becoming a terminal-shaped Cuenv UI
whose underlying terminal is still unsafe for editors, SSH, TUIs, and long
running work.

## Starting point

Cuetty already has a real Rio-backed shell, resizing, keyboard input,
clipboard paste, OSC 52 writes, mouse text selection and copy, a visible-frame
literal search, independent local tabs, a native macOS window, and an
asynchronous per-directory CUE presentation. Its pure workspace and
persistence contracts are also more mature than the live product shell.

| Area | Current state | What blocks daily use |
| --- | --- | --- |
| Shell, resize, ANSI colour, tabs | Working proof of concept with an interactive macOS shell/history smoke pass | Needs the real-workload acceptance corpus and broader regression coverage. |
| Text model | Complete backend cluster text, wide-cell placeholders, and observed cursor state | New direct-Rio path needs executable and macOS validation; full Unicode and IME acceptance remain open. |
| Input protocol | Direct-host input and mode-aware bracketed-paste encoding in source; local selection | Executable compatibility checks remain; mouse, keypad, focus and advanced keyboard integration are not qualified. |
| Scrollback and search | Rio-owned wheel/keyboard history navigation is live; the UI searches only the visible frame | No selection/search across history or scrollbar. |
| Tabs and splits | Each tab owns a live session; the pure layout model supports splits | Split shortcuts are intentionally rejected because session allocation is not wired. |
| Settings | Font size and line-height controls are session-only | No durable configuration, font validation, theme selection, or stable working-directory policy. |
| Distribution | Development bundle with ad-hoc signing and a local launch check | No signed/notarized release, installer, update path, or release-quality macOS CI. |

The original `librio` pin exposed only one `char` per snapshot cell and left
combining clusters and cursor shape/visibility unobservable to Cuetty. The
direct `rio-vt` implementation removes that particular frame boundary; it
does not establish compatibility or daily-driver readiness by itself.

The engine decision is now accepted: Cuetty is moving directly to
`rio-vt` behind `TerminalSession`, rather than retain `librio` as its Rust
terminal boundary. See
[ADR-0010: Direct rio-vt Integration for Cuetty](/decisions/adrs/adr-0010-cuetty-direct-rio-vt-integration/).

## Scope and non-goals

The initial daily-driver target is:

- macOS on Apple Silicon;
- a local login shell, SSH, editors, CLI tools, and common TUIs;
- copy, paste, history, search, tabs, splits, safe close, and sane persistent
  settings;
- an application bundle that can live in `/Applications` and be launched from
  Dock, Spotlight, or a hotkey.

It does not initially include Windows, Linux, a plugin marketplace, remote
session reconnect, terminal image protocols, custom shaders, or automatic
updates. Those are separate product bets. None should delay the daily-driver
gate.

Likewise, workspace restoration should initially mean restoring a layout and
starting fresh shells. It must not imply that a local shell or a running job
survives an app restart. Durable session recovery belongs to a later tmux or
similar backend.

## Milestones

Effort ranges below are engineering effort, not calendar promises. The Rio
qualification result is the largest source of uncertainty.

### M0: qualify the selected terminal integration

**Goal:** within ten working days of engineering effort, prove that one Cuetty
pane backed by a pinned `rio-vt` revision can support the terminal behaviours
needed to replace a vanilla Ghostty session, using only public `rio-vt` APIs
and without Cuetty implementing a second parser, mode tracker, or scrollback
store. Direct `rio-vt` fails qualification only if a must-have behaviour cannot
be supplied by Rio's core or public integration surface, or a reproducible
Rio-core defect blocks that behaviour, when the timebox expires. Remaining
Cuetty host or renderer implementation gaps are recorded for M1 and do not, by
themselves, disqualify Rio.

The qualification is a pass only when all of these results are recorded:

- executable fixtures pass for grapheme and wide-cell output, scrollback,
  cursor state, bracketed paste, application cursor and keypad modes, mouse
  reporting, hyperlinks, resize, final-output drain, and child exit status;
- the existing PTY, frame, input, resize, and close regression suite passes
  through `TerminalSession` against both the new adapter and the retained
  `librio` control case where their capabilities overlap;
- an Apple Silicon macOS smoke run exercises a local login-shell workflow, an
  SSH workflow, a full-screen editor, `fzf`, and a Kubernetes or coding-agent
  TUI, with every failure attributed to either Rio core/public integration or
  Cuetty host/renderer work and no terminal-breaking Rio failure unresolved;
  and
- a machine-readable report identifies the exact engine revision and marks
  every required capability as pass, fail, or unobservable, with no required
  item left unobservable for promotion.

This M0 scope is deliberately one pane and one process lifecycle. Rendering
polish, persistent settings, tabs, splits, packaging, and Cuenv-specific UI
remain in later milestones and cannot be used to extend the qualification
timebox.

- [x] Decide the integration direction: retain Rio's terminal core, but move
  Cuetty from the C-oriented `librio` wrapper to direct `rio-vt` integration.
  The decision and rationale are recorded in
  [ADR-0010](/decisions/adrs/adr-0010-cuetty-direct-rio-vt-integration/).
- [ ] Create a small compatibility spike against the chosen `rio-vt` revision behind
  `TerminalSession`. Verify, from public API only: grapheme cells, scrollback
  access, cursor state, bracketed paste, application cursor/keypad modes,
  mouse reporting, hyperlinks, and child lifecycle.
- [ ] Add a machine-readable capability report and an adapter fixture for every
  supported item. Keep the current `librio` pin as the control case.
- [ ] Do not add a second parser, scrollback store, or mode tracker in Cuetty
  merely to compensate for an unobservable Rio state.

#### Implementation checkpoint: 7 September 2026

The first direct-`rio-vt` implementation is in progress against revision
`7ae087500bcde5c0c9f09cb9c50382e9220b3360`. The following frame/model work is
implemented in source, but M0 has **not** passed:

- A direct `Crosswords` snapshot adapter copies visible cells, resolves styles
  with row metadata, and reads cluster extras and cursor state while the
  session holds one terminal lock. Inline background colours are decoded
  separately from style IDs. Every copied row is conservatively dirty; no
  stable content revision is invented.
- Session capability metadata advertises the implemented bracketed-paste
  encoder, not general Rio protocol support. Scrollback access, mouse
  reporting and hyperlink destinations remain unsupported in the host
  contract even where Rio's core exposes the underlying state.
- `TerminalCell::text` preserves the complete backend-declared cluster.
  GPUI shaping, selection/copy, and literal search consume the full text;
  search results still address physical cells, not combining-mark fragments.
- Parser-driven regression fixtures cover combining and wide text, cursor
  visibility/shape, alternate-screen style isolation, inline backgrounds,
  and scrolled-viewport cursor suppression. Model and search/copy fixtures
  cover nonempty cluster text and physical-cell coordinates.
- Session wake handling samples a final frame before acting on a drained
  close event. This makes final PTY output sampleable at the session boundary;
  the UI still removes the pane, so it does not promise visible retained
  output after process exit. A focused registry regression covers this order.

At this checkpoint these additions still need app compilation and execution
of the focused tests. The real PTY lifecycle suite, final-output drain, child
exit status, and Apple Silicon smoke corpus remain unvalidated. IME, focus
reporting, application keypad and advanced keyboard behaviour, and hyperlink
destinations remain open qualification items. The existing `librio` baseline
is a comparison requirement, not evidence that a dual-backend regression
run has happened. No daily-driver or full Unicode acceptance is claimed.

#### Implementation checkpoint: 10 September 2026

Cuetty is pinned to `b0694c0707a90dc93fdf01cbf8a658424be285ee`, the
head of [Rio PR #1927](https://github.com/raphamorim/rio/pull/1927). The change
fixes the two Rio-owned blockers found at the earlier qualification revision:
buffered final output now survives terminal-lock contention, and Unix PTY
shutdown retires/reaps the owned child without signalling a recycled PID.

On Apple Silicon macOS, the formerly failing contention regression and the
serial real-PTY input, paste, resize, final-output/exit-status, and bounded-reap
fixtures pass. This is focused capability evidence, not the complete M0 gate:
the real-workload corpus, mouse/focus/keypad protocols, hyperlinks, IME, and the
retained librio control comparison remain open.

Rio-owned history navigation is now wired through `TerminalSession` to
wheel/trackpad scrolling and Shift-Page Up/Down or Shift-Home/End. A real-PTY
fixture proves navigation to retained history and back to the live viewport.
Selection and literal search still address only one sampled viewport, so a
viewport move clears their coordinates rather than silently copying or
highlighting different text.

The ad-hoc-signed app bundle also launched into a visible shell. Direct command
input produced output, Shift-Page Up reached older output, Shift-End returned to
the live viewport, and subsequent input still rendered. This is a synthetic UI
smoke check, not evidence for SSH, a full-screen editor, `fzf`, or a Kubernetes
or coding-agent TUI.

A follow-up launch exposed a host failure when the interactive shell negotiated
an extended keyboard mode: the next keypress replaced the terminal with a fatal
error. Cuetty now reads Rio's live Kitty and `modifyOtherKeys` state and encodes
the representable keypress/modifier/named-key/repeat subset. Deterministic unit
and real-PTY fixtures cover that negotiation path. Key releases, keypad
identity, alternate-key reporting, and IME commits remain open.

The same follow-up normalizes inherited `SHLVL` at the PTY boundary. A Cuetty
window is a new top-level shell session and now starts at level 1 even when its
development launcher was itself nested; a real-PTY fixture covers that contract.

**Exit gate:** the project has a pinned engine whose public surface can support
the M1 contract, plus a migration test that proves the existing PTY, frame, and
close semantics still work.

**Why first:** the original frame could not represent the text and input
behaviours that make a terminal trustworthy. The replacement must prove those
contracts before product-shell work depends on them.

### M1: terminal correctness and compatibility

**Goal:** make one session correct before adding more product shell.

- [ ] Qualify the new renderer-neutral `TerminalCell::text` cluster model.
  Source now preserves cell width, continuation state, styles, wrapping, and
  cursor position without leaking Rio types into GPUI; hyperlink destinations
  and executable/macOS acceptance are still outstanding.
- [x] Give scrollback one authoritative owner. The renderer should receive a
  viewport over terminal history, not reconstruct a second transcript from
  painted frames. This is essential for alternate screens, reflow, selection,
  and search.
- [ ] Map and test terminal input modes: application cursor and keypad keys,
  bracketed paste, focus events, mouse reporting, and the precedence between
  application mouse input and host selection.
- [ ] Choose a conservative `TERM` and terminfo policy. Advertise
  `xterm-256color` until every extra capability is proven. Do not claim Kitty
  keyboard or graphics support because an application happens to start.
- [ ] Add a deterministic terminal conformance corpus. It should cover ANSI
  colour/style, wrapping, resize, Unicode and emoji, OSC title and clipboard,
  alternate screen, input modes, child exit, and high-output damage handling.
- [ ] Add a macOS manual acceptance script using the actual daily workload:
  shell editing, `ssh`, an editor, `fzf`, `tmux` if used, and a Kubernetes TUI
  such as `k9s` if used. Capture screenshots or interaction transcripts for
  failures.

**Exit gate:** the fixture suite passes on the supported macOS build; `vttest`
and the selected real-tool corpus have no terminal-breaking behaviour; a long
Unicode transcript, a full-screen TUI, and an SSH session all work without
falling back to Ghostty.

### M2: comfortable normal terminal work

**Goal:** close the behaviours a mostly vanilla terminal user notices every
day.

- [ ] Wire full scrollback into GPUI: mouse wheel and trackpad scrolling,
  Page Up/Down, a clear follow-output state, and an unobtrusive scrollbar.
- [ ] Search the full transcript incrementally. Keep literal search first;
  introduce regex only after its performance and match-navigation contract are
  clear.
- [ ] Finish selection: word and line selection, copy from history, multi-line
  Unicode correctness, and a predictable policy when an application has mouse
  reporting enabled.
- [ ] Persist a versioned configuration in the platform application-support
  location. Start with font family and fallbacks, font size, line height, theme,
  cursor preference, scrollback limit, window geometry, and working-directory
  policy.
- [ ] Treat the current hard-coded Monaspice/Catppuccin defaults as an
  opinionated first-run profile, not a dependency. Detect unavailable fonts and
  fall back to a macOS monospaced font with a visible settings error.
- [ ] Provide familiar macOS actions before inventing a keybinding language:
  new tab, close tab, copy, paste, find, next/previous result, zoom, and reset
  zoom.
- [ ] Make new-tab location a user choice. Preserve the existing `$HOME`
  behaviour as an option, then support active-directory inheritance and an
  explicit configured directory. Do not silently change a user's session
  semantics.
- [ ] Ask before closing a tab or window that has a running foreground process,
  then make the outcome and error state visible.

**Exit gate:** a user can work through a long coloured log, select and copy an
older result, search it, adjust a font once, restart Cuetty, and get the same
usable setup back.

### M3: finish the local macOS shell

**Goal:** make the existing tab and workspace model useful rather than merely
present.

- [ ] Allocate a live session before committing a split to the workspace tree.
  On creation failure, leave the tree untouched. The current pure layout model
  already gives this work a good test seam.
- [ ] Implement horizontal and vertical splits, focus traversal, pane resizing,
  close/reopen, tab reordering, and a compact tab overview. Match macOS
  conventions where possible rather than copying every Ghostty action.
- [ ] Persist layout metadata and restore it only as fresh local shells. Keep
  the session identity seam ready for a future attachable backend, but do not
  pretend local processes survived a restart.
- [ ] Improve lifecycle handling: title updates, bells/notifications, child
  exit state, last-window behaviour, and a clear empty-window experience.
- [ ] Evaluate a Quick Terminal only after the ordinary app is dependable. It
  is a convenience feature, not a prerequisite for replacing a vanilla
  Ghostty setup.

**Exit gate:** tab and split operations are atomic in unit tests, survive a
clean application restart as layout metadata, and do not lose or misroute a
live pane during resize or close.

### M4: make it installable and run it as the primary terminal

**Goal:** turn a development build into a reversible daily-driver release.

- [ ] Produce versioned arm64 macOS application bundles without requiring a
  local Rust toolchain or Metal download on the user machine.
- [ ] Replace ad-hoc signing with Developer ID signing, hardened runtime,
  notarization, and a verification job. Keep entitlements minimal and review
  them as part of each release.
- [ ] Add macOS CI that builds the bundle, runs focused terminal tests, signs a
  test artifact, and records the engine revision, dependencies, and licence
  report.
- [ ] Choose a simple first installer, such as a signed GitHub release plus a
  Homebrew cask. Manual update is acceptable at this stage; automatic update
  can wait until the signing and rollback story is mature.
- [ ] Keep Ghostty installed during a staged canary: one short session, then a
  half day, then three working days, then two weeks as the primary terminal.
  Record blockers, crashes, terminal incompatibilities, and any data-loss or
  close-safety incident.
- [ ] Only promote Cuetty in Dock, Spotlight habits, and any terminal hotkey
  after the two-week canary has no severity-one blocker and no unexplained
  compatibility regression.

**Exit gate:** Cuetty can be installed and restored without a development
environment; the canary completes with no lost work and Ghostty is no longer
needed for routine terminal tasks.

### M5: add the Cuenv advantage

**Goal:** make Cuetty meaningfully better for Cuenv projects while keeping the
terminal useful everywhere else.

- [x] Keep the existing per-directory banner and border path bounded,
  asynchronous, and last-known-good.
- [ ] Add a redacted project context surface: project name, selected
  environment, task status, and safe diagnostics. Never render resolved secrets
  or make configuration evaluation block input or painting.
- [x] Add exact-CWD Cuenv task discovery to a Command-K palette and floating
  sidebar. Launch through the canonical `cuenv task` CLI in the detecting pane
  so terminal cancellation and exit semantics stay visible; stage tasks with
  required parameters for editing.
- [ ] Link errors and task output to source locations when they are available,
  but do not couple the core terminal to a specific editor.

**Exit gate:** Cuenv projects gain useful context and task affordances, while a
directory without Cuenv configuration remains an ordinary fast terminal.

## Prioritized implementation queue

Create and complete these issues in order. Each issue should include focused
tests and an observed macOS result, not only unit-test output.

1. `cuetty: qualify current Rio API and decide the engine pin`
2. `cuetty: add terminal conformance fixtures and real-tool acceptance corpus`
3. `cuetty: introduce grapheme-aware terminal frame v2`
4. `cuetty: expose authoritative history and viewport contract`
5. `cuetty: bridge terminal input and mouse modes`
6. `cuetty: wire history, selection, and full-transcript search into GPUI`
7. `cuetty: persist configuration and validate fonts on macOS`
8. `cuetty: implement safe close and child lifecycle UI`
9. `cuetty: allocate live sessions transactionally for splits`
10. `cuetty: persist and restore local workspace metadata`
11. `cuetty: produce a signed, notarized macOS release`
12. `cuetty: run the two-week daily-driver canary and triage every blocker`

## Evidence required for promotion

The project should not mark a milestone complete because a feature exists in a
demo. For each milestone, record:

- the adapter capability relied on and its source-level test;
- focused unit and integration results;
- a macOS live interaction result or screenshot;
- the exact engine revision and dependency/licence delta;
- unsupported behaviours that remain deliberately out of scope;
- any canary blocker and its resolution or explicit deferral.

This makes the decision to replace Ghostty evidence-based and keeps Cuetty
small, native, and recognisably a Cuenv product rather than a stalled attempt
to reproduce every terminal emulator feature.
