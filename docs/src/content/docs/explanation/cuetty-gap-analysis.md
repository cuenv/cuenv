---
title: Cuetty gap analysis
description: Gap analysis between Termy, Okena, and Cuetty
---

This analysis compares Cuetty with the Termy and Okena reference projects.
They are learning references, not vendored code. Cuetty's terminal engine is
the pinned Rio revision `b0b79c1ebadc8d6a9a79c4c44a91a42b3ea439d1`, hosted in a
Rust/GPUI application.

For the deliberately narrower macOS path from a vanilla terminal workflow to
Cuetty as a daily driver, see the [Cuetty daily-driver roadmap](/explanation/cuetty-daily-driver-roadmap/).

## Snapshot

| Area | Termy/Okena reference pattern | Cuetty today | Remaining gap |
| --- | --- | --- | --- |
| Terminal core | Mature terminal runtime and renderer with broader protocol coverage. | Rio-backed session, semantic frame adapter, fixed-grid GPUI renderer. | More protocol, Unicode, scrollback, and lifecycle coverage. |
| Rendering | Fixed cell geometry, shaping, cursor and selection behaviour tuned for daily use. | Fixed-grid rows and cells with wide-cell, soft-wrap, semantic-style, and cursor policies. | Human visual acceptance and broader glyph/IME coverage. |
| Interaction | Selection, clipboard, search, scrolling, and keyboard modes integrated into the terminal surface. | Input, resize, selection/copy, and visible-frame literal search are live. | Full scrollback search, regex, keyboard modes, and IME. |
| Product shell | Mature tabs, panes, commands, settings, persistence, and notifications. | Real GPUI tabs with one independent Rio session per tab; split actions remain rejected until pane/session allocation exists. | Wire splits and the remaining daily-use shell. |
| Persistence | Saved layouts and richer app state. | Versioned metadata-only workspace codec/storage traits. | Live restore/reconnect and durable file integration. |
| Extensibility | Project-specific command and integration surfaces. | Capability and session-backend traits with deny-by-default scopes. | Remote, tmux, Wasm, and plugin runtimes. |
| Platform | Reference coverage varies by project. | macOS/Metal runtime verified only. | Linux and Windows validation. |

## Priority gaps

### P0: Single-terminal correctness

The single Rio-backed terminal is the critical path. The next correctness work
should cover:

- Full scrollback ownership, viewport offsets, scrollbar affordances, and
  search over the complete transcript.
- Application cursor/keypad modes, extended keyboard behaviour, mouse
  reporting, alternate-screen handling, bracketed paste, and protocol replies.
- Complete Unicode width and shaping behaviour, including combining marks,
  ZWJ sequences, emoji presentation, and IME composition.
- Child lifecycle, exit UI, title/bell/notification events, and resize
  coalescing under high output.
- Human visual/input acceptance on the supported macOS runtime.

### P1: Daily terminal features

The pure interaction and workspace contracts exist, but the live shell still
needs:

1. Full-history scrolling and search, including regex only after a clear
   search contract exists.
2. Independent Rio sessions for each live split (tabs are already session-backed).
3. Close/reopen lifecycle, titles, pane resizing, and live workspace restore.
4. Configurable shell, working directory, environment policy, font metrics,
   theme, scrollback, cursor, and keybindings.

### P2: Product shell

Termy and Okena demonstrate useful shell features, but Cuenv should add them
only where they serve Cuenv workflows:

- command/task discovery and launch;
- environment activation visibility without exposing secret values;
- task history and cache-aware rerun affordances;
- navigation between terminal output, task definitions, and documentation;
- notifications and a command palette after the command catalog is stable.

Theme stores, auto-update infrastructure, broad plugin marketplaces, and
general-purpose agent surfaces are not current Cuetty requirements.

### P3: Integration seams

The capability and session-backend traits are intentionally ready for future
implementations, but no unsupported backend is being advertised:

- tmux and remote sessions require an explicit backend contract;
- attach/reconnect requires durable session identity and lifecycle semantics;
- Wasm plugins require a versioned, permissioned host ABI and are not present;
- component-tree rendering must remain host-controlled rather than exposing
  GPUI internals to untrusted extensions;
- graphics protocols require renderer and resource-lifetime design.

## Recommended order

1. Finish per-session Rio correctness and human visual/input acceptance.
2. Implement independent Rio session allocation and lifecycle for splits; tabs
   already own independent sessions.
3. Wire full-history scrollback, search, configuration, and workspace restore.
4. Add Cuenv task/environment affordances behind the existing traits.
5. Qualify Linux and Windows builds separately; do not infer support from Rust
   compilation alone.
6. Evaluate tmux, remote, and Wasm backends as separate projects with their
   own security and compatibility gates.

This sequence keeps the app shell substrate-agnostic while making Rio the
active, clearly bounded terminal engine.
