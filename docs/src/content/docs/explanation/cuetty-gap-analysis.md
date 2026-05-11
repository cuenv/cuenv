---
title: Cuetty gap analysis
description: Gap analysis between the Termy reference checkout and Cuetty
---

This analysis compares the local `apps/cuetty/termy` reference checkout with
the current Cuetty app. Termy is a useful product reference, but Cuetty is not
intended to become a Termy fork. The main architectural difference is deliberate:
Cuetty uses GPUI with the Ghostty VT stack, while Termy uses GPUI with
`alacritty_terminal` and a large app-owned terminal runtime.

## Snapshot

| Area | Termy reference | Cuetty today | Gap |
| --- | --- | --- | --- |
| Scale | Multi-crate workspace with app, terminal UI, config, command, search, theme, update, CLI, toast, and API crates. | Standalone Rust app with `lib`, `ui`, `pty`, and terminal response modules. | Cuetty is still a focused prototype, not a product shell. |
| Terminal core | `alacritty_terminal` plus `termy_terminal_ui` for PTY runtime, grid extraction, protocol replies, scrollback, mouse, keyboard, links, shell integration, and tmux. | `gpui_ghostty_terminal` plus `portable-pty`, with a small Device Attributes responder. | Cuetty needs single-terminal correctness work before app-level parity. |
| App model | Tabs, panes, tmux runtime, command palette, settings, config reload, saved layouts, tasks, search, toasts, auto-update, theme store, CLI companion, and onboarding. | One GPUI window running one login shell. | Most user-facing terminal product features are absent. |
| Configuration | Generated config reference with appearance, terminal, tabs, keybindings, tasks, colors, startup, safety, and updates. | Process options are Rust structs; no user config file or live reload. | Cuetty needs a configuration boundary before features multiply. |
| Validation | Large test corpus across core parsing, runtime, rendering, config, search, tmux, commands, and app behavior. | Focused unit tests around identity, shell fallback, grid sizing, and DA responses. | Cuetty has test coverage for the current scaffold only. |
| Distribution | Installer scripts, bundle metadata, assets, auto-update, release core, CLI install flow, and roadmap items for signing and package distribution. | Nix build/run/check only, with app-local flake ownership. | Distribution is intentionally minimal. |

## Gaps to Close First

### P0: Single-Terminal Correctness

Cuetty should not copy Termy's app surface until the single terminal view is
solid. The high-value gaps are:

- Keyboard protocol coverage, including application cursor/keypad modes and
  extended keyboard behavior.
- Mouse reporting, wheel scrolling, drag selection, and alternate-screen
  behavior.
- Scrollback ownership, display offset handling, and scrollbar affordances.
- Selection, copy, paste, bracketed paste, and clipboard query/store behavior.
- Terminal title, bell, exit, notification, working-directory, and progress
  events.
- A documented protocol-reply strategy for Ghostty-backed rendering. Cuetty
  currently patches Device Attributes locally because the pinned upstream stack
  does not answer those queries yet.

### P0: Process Runtime and Resize Semantics

Termy has a full PTY runtime with polling, batched event draining, child exit
handling, resize delivery, working-directory resolution, shell integration, and
platform-specific shell selection. Cuetty currently has straightforward reader
and writer threads around `portable-pty`.

Cuetty needs:

- Platform-aware shell resolution and shell arguments.
- Configurable working directory and environment policy.
- Explicit child process lifecycle and exit UI.
- Resize throttling or coalescing.
- Backpressure and batching rules for high-output commands.
- A clear boundary between PTY runtime, Ghostty terminal session, and GPUI view.

### P1: Configuration and Commands

Termy has a pure command/keybind core plus app adapters, generated docs, and a
settings UI. Cuetty only has hard-coded quit/copy/paste/select-all bindings.

Cuetty needs:

- A small config file format for shell, working directory, font, theme, scrollback,
  and keybindings.
- A command catalog before adding more GPUI actions.
- Deterministic keybinding resolution and generated documentation.
- A command palette only after the command catalog exists.

### P1: Terminal UX Surface

Termy already has tabs, panes, split management, tab titles, search, scrollbars,
saved layouts, and tmux session management. Cuetty should add these in stages:

1. Tabs with title handling and per-tab process lifecycle.
2. Search and scrollback navigation.
3. Optional panes or tmux integration, not both at once.
4. Saved layouts only after tabs and panes are stable.

### P1: Appearance and Rendering

Termy owns its rendering path, which gives it damage snapshots, cached grid
painting, render metrics, cursor behavior, configurable padding, themes,
opacity, blur, and scrollbars. Cuetty delegates rendering to
`gpui_ghostty_terminal`, so the gap is partly feature work and partly upstream
capability assessment.

Cuetty needs:

- Font family, font size, line height, padding, and theme configuration.
- Cursor style and blink policy.
- Render performance metrics for high-output workloads.
- A decision on whether missing rendering controls belong upstream in
  `gpui_ghostty_terminal` or locally in Cuetty.

### P2: Product Shell

Termy includes features that are useful references but not obvious Cuetty
requirements:

- Auto-update and release infrastructure.
- Theme store, deeplinks, and marketplace-oriented flows.
- Plugin runtime.
- API server.
- Agent workspace features.
- Onboarding and installer scripts.

These should stay out of Cuetty until the cuenv-specific product goal requires
them. They add maintenance load without improving the terminal core.

## Termy Gaps That Still Matter

Termy is much more complete, but its roadmap still lists v1 blockers and
non-trivial gaps: signing and notarization, placeholder bundle metadata, OSC
support, platform parity, multi-window support, Ghostty config compatibility,
font ligatures, image protocols, crash reporting, benchmark CI, accessibility,
and launch documentation. Matching Termy is therefore not the same as reaching
terminal-emulator maturity.

## Native Ghostty Embedding References

Supacode and Limux are more useful architecture references than Termy for the
terminal core because both avoid reimplementing terminal-emulator behavior in
the host app. They embed Ghostty's native app/surface API and keep their own
code focused on product state around those surfaces.

### Supacode

Supacode is a useful Ghostty reference, but it is not using the same integration
path as Cuetty. It builds Ghostty's macOS `GhosttyKit.xcframework` from a
`ThirdParty/ghostty` submodule with Zig, then imports `GhosttyKit` directly from
Swift. Its app creates one shared `ghostty_app_t` runtime and creates independent
`ghostty_surface_t` instances for tabs and splits.

The notable pattern is that Ghostty owns the terminal surface, terminal input
translation, scrollback, selection, clipboard protocol, search, keybindings, and
terminal actions. Supacode wraps those actions in its product model: worktrees,
tabs, split trees, scripts, notifications, and command palette entries. App-level
tab and split actions are triggered through Ghostty binding actions so user
Ghostty keybindings stay authoritative.

This is easier than Cuetty's current route in one important way: Supacode embeds
Ghostty's native app/surface API rather than reconstructing a terminal runtime
around a GPUI renderer crate. The tradeoff is platform scope. Supacode is
macOS-only and Swift/AppKit-based, while Cuetty is a Rust/GPUI app that currently
targets an app-local flake across macOS and Linux systems.

### Limux

Limux is closer to Cuetty's language and Linux runtime, but still not the same
UI stack. It builds Ghostty as `libghostty.so` with
`zig build -Dapp-runtime=none -Doptimize=ReleaseFast`, exposes a small
`limux-ghostty-sys` Rust crate with raw FFI bindings, links `ghostty` and
`epoxy`, and hosts each terminal in a GTK `GLArea`.

Its startup path sets the embedded Ghostty runtime environment
(`GHOSTTY_RESOURCES_DIR`, `TERMINFO`, and
`GHOSTTY_SHELL_INTEGRATION_XDG_DIR`), disables incompatible GTK render paths,
calls `ghostty_init`, loads Ghostty config, and creates one shared
`ghostty_app_t`. Each pane or tab creates a `ghostty_surface_t` with working
directory, startup command, extra environment, content scale, and a surface
context.

Limux then treats Ghostty as the terminal engine:

- GTK render callbacks call `ghostty_surface_draw`.
- Resize callbacks call `ghostty_surface_set_content_scale` and
  `ghostty_surface_set_size`.
- Focus, keyboard, IME, mouse, scroll, and file-drop events are translated into
  Ghostty surface calls.
- Ghostty actions drive host UI updates for render requests, scrollbars, title,
  current working directory, desktop notifications, bell, child exit, and config
  reload.
- Copy, paste, clear, search, scroll-to-row, and font-size changes go through
  `ghostty_surface_binding_action` instead of app-owned terminal protocol code.

The important lesson is that a Rust app can delegate much more terminal behavior
to embedded Ghostty than Cuetty currently does. The cost is not zero: Cuetty
would need an unsafe FFI boundary, resource and terminfo packaging, platform
render-surface glue, runtime config handling, callback dispatch, and a GPUI host
for the Ghostty surface lifecycle. Limux solves that for GTK/OpenGL on Linux; it
does not solve GPUI integration directly.

## What Cuetty Should Learn

The common pattern from Supacode and Limux is:

1. Own product concepts in the app.
2. Let Ghostty own terminal concepts.
3. Bridge terminal events into product state through a small callback layer.
4. Send terminal commands back through Ghostty binding actions where possible.

For Cuetty, that means the product roadmap should be smaller than Termy's and
more cuenv-specific. The terminal substrate should become boring and delegated;
the unique work should be task, environment, and workspace workflows that a
general terminal does not know about.

Cuetty-specific features worth protecting as unique product work:

- cuenv task discovery, task launch, and task history.
- environment activation visibility, including changed variables and secret
  resolution state without leaking secret values.
- cache-aware task output and rerun affordances.
- workspace/package context from cuenv's project graph.
- direct navigation between terminal output, task definitions, and docs.
- optional agent/workflow hooks only where they map to cuenv tasks and
  environments.

## Recommended Cuetty Roadmap

1. **Choose the Ghostty integration boundary.** Either keep hardening
   `gpui_ghostty_terminal`, or prototype a native embedded-Ghostty adapter for
   GPUI modeled after Supacode and Limux. This decision changes most downstream
   terminal work.
2. **Keep the app shell substrate-agnostic.** Cuetty now has app-owned tabs,
   split trees, and terminal pane records around the current
   `gpui_ghostty_terminal` substrate. Keep new app features behind that boundary
   so a native embedded-Ghostty adapter can replace the pane implementation later.
3. **Make the single terminal boring.** Finish input, mouse, scrollback,
   selection, paste, query replies, resize, and lifecycle behavior for one
   terminal, but delegate to Ghostty wherever the embedded API can own it.
4. **Add a small configuration and command boundary.** Keep it pure and tested
   before wiring more UI actions.
5. **Promote the first tab and split model to daily-use quality.** The current
   slice creates live tabs and right/down splits, but still needs pane close,
   layout persistence, better titles, and manual resize controls.
6. **Evaluate tmux separately from native panes.** Termy supports both ideas,
   but Cuetty should choose based on cuenv workflows instead of copying surface
   area.
7. **Only then invest in distribution polish.** App icons, installers,
   auto-update, and signing matter after the daily terminal loop is reliable.

## Open Decisions

- Should Cuetty expose Ghostty-style configuration, a cuenv-native format, or a
  small app-specific config first?
- Should tabs and panes be native Cuetty concepts, tmux-backed concepts, or a
  deliberately smaller subset?
- Which missing Ghostty terminal controls should be contributed upstream to
  `gpui_ghostty_terminal` instead of maintained locally?
- Is Cuetty a general-purpose terminal app, or a cuenv-focused terminal with
  task/environment affordances?
