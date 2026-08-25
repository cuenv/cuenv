---
title: Cuetty
description: Cuetty terminal app architecture and development workflow
---

Cuetty is the experimental cuenv terminal app in `apps/cuetty`. It is a
standalone Rust desktop application using GPUI for the product shell and the
pinned Rio terminal engine for terminal state, parsing, PTY integration, and
semantic render frames.

The active Rio revision is:

`b0b79c1ebadc8d6a9a79c4c44a91a42b3ea439d1`

That pin is intentional. Cuetty uses the public APIs available at this
revision rather than depending on private renderer or embedding APIs.

## Architecture

The boundary is deliberately replaceable:

- Rio owns terminal parsing, PTY-backed session state, terminal actions,
  semantic colours and styles, cursor state, cell occupancy, wrapping, and
  resize semantics.
- Cuetty's terminal model translates Rio state into a renderer-neutral frame
  protocol. GPUI does not receive Rio types directly.
- GPUI owns the window, title bar, fixed-grid cell renderer, focus, input
  routing, selection and copy overlay, visible-frame search overlay, and
  workspace shell.
- Rust traits define terminal sessions, frame snapshots, configuration,
  workspace persistence, session backends, and capability checks so those
  pieces can be replaced or extended independently.

The current usable slice includes:

- A login shell attached to a Rio-backed terminal session.
- Fixed-width, fixed-row-height GPUI rendering with semantic colours and
  explicit wide-cell and soft-wrap occupancy.
- Catppuccin Mocha is the built-in terminal colour scheme shared by Rio's
  semantic resolver and the GPUI chrome.
- Ghostty is the visual reference for restrained native chrome: traffic lights,
  one title strip, stable single-line tab labels, and a full-bleed terminal.
- Keyboard input, paste, resize, title updates, clipboard copy, mouse
  selection, and visible-frame literal search.
- Window and rail geometry are mapped to the active pane before each Rio
  resize, so changing either boundary recalculates terminal rows and columns
  instead of leaving a stale frame clipped in the viewport.
- The terminal surface keeps an 8px inset from the host chrome; the inset uses
  the same terminal surface colour and is subtracted from the Rio grid
  dimensions as well as the rendered bounds.
- Independent live Rio sessions for each tab with a replaceable workspace
  model. Cmd-T creates a session-backed tab and Cmd-W closes the active one;
  split shortcuts are rejected with a visible notice until pane/session
  allocation is implemented, and the UI never presents a fake terminal pane.
- A vertical GPUI Component tab rail that resizes from 50px to 280px. The
  50px compact mode shows tab ordinals with title tooltips, and the collapse
  control restores the last expanded width.
- A session-only Settings destination with transactional Apply, Cancel, and
  Reset controls for font size and line height. The native Cuetty menu exposes
  Quit Cuetty and Cmd-Q uses the same application-level action.
- Versioned, metadata-only workspace persistence and capability/session seams
  for later product integrations.

Termy and Okena remain reference projects for renderer and product-shell
patterns. They are not vendored dependencies and their runtime models are not
silently substituted for Rio.

## Current limits

The following are deliberately documented as staged work rather than implied
support:

- Full scrollback navigation and full-history search; current search is over
  the visible frame and supports literal matching only.
- Complete Unicode behaviour, including combining marks, ZWJ sequences,
  emoji presentation, and IME correctness.
- Kitty graphics and other image protocols.
- tmux, remote, attach, and reconnect backends.
- Wasm plugins and a plugin/component-tree runtime.
- Split-pane session allocation, plus live workspace restore/reconnect.

The implementation has focused Rust validation and signed macOS bundle
verification. Human visual and input acceptance remains a separate caveat:
the terminal must still be exercised interactively for font metrics, line
spacing, selection, search focus, clipboard behaviour, resize, and shell
lifecycle before a release claim. macOS with Apple's Metal toolchain is the
only runtime-verified platform at present; Linux and Windows are unverified.

## Development

Use the app-local workflow from `apps/cuetty`. The app remains standalone and
does not participate in the root cuenv release pipeline until it is ready:

```bash
cargo fmt -- --check
cargo check --locked
cargo test --locked
cargo clippy --all-targets --all-features -- -D warnings
cargo build --release --locked
./script/build_and_run.sh --verify
```

`build_and_run.sh --verify` uses the installed macOS Metal toolchain to build,
sign, and validate the application bundle. The exact commands and resulting
platform boundary should be reported with every Cuetty implementation change.
