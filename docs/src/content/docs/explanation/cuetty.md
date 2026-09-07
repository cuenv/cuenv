---
title: Cuetty
description: Cuetty terminal app architecture and development workflow
---

Cuetty is the experimental cuenv terminal app in `apps/cuetty`. It is a
standalone Rust desktop application using GPUI for the product shell and the
pinned Rio terminal engine for terminal state, parsing, PTY integration, and
semantic render frames.

The direct-`rio-vt` qualification revision is:

`7ae087500bcde5c0c9f09cb9c50382e9220b3360`

That pin is intentional. Cuetty uses the public APIs available at this
revision rather than depending on private renderer or embedding APIs.

The original proof of concept used `librio` at
`b0b79c1ebadc8d6a9a79c4c44a91a42b3ea439d1`. The M0 implementation migrates to
direct `rio-vt` integration behind the existing session seam;
see [ADR-0010: Direct rio-vt Integration for Cuetty](/decisions/adrs/adr-0010-cuetty-direct-rio-vt-integration/).
Promotion is gated by the documented qualification spike and does not imply
a Rio engine replacement. The new source preserves full cell-cluster text
through rendering, copy, and visible-frame literal search, and samples
observed cursor state and row-aware styles under one terminal lock. These
changes are not yet qualification-complete; see the
[M0 implementation checkpoint](/explanation/cuetty-daily-driver-roadmap/#implementation-checkpoint-7-september-2026).

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
  every newly-created session starts in `$HOME`,
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

## Per-directory Cuenv presentation

For the active Rio tab, Cuetty asks Rio for the shell's current working
directory and evaluates package `cuetty` in that exact directory. The nearest
`cue.mod/module.cue` establishes the module root only; configuration is not
inherited from parent directories and any `.cue` filename may provide the
package. A directory without package `cuetty` is neutral. Standalone packages
are evaluated in the exact directory; imports that require a module context
fail through normal CUE evaluation. A matching package may provide a
single-line `banner` and a quoted `border: "#RRGGBB"`; for
example `border: "#ff0000"` draws a thin selected-pane and tab accent.

Directory and module-metadata changes are watched natively. Evaluation and
validation occur off the GPUI thread. Invalid matching CUE keeps the last
valid presentation for that directory and exposes an integration notice.

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

Earlier implementation work recorded focused Rust validation and macOS bundle
verification. Those results do not validate the new direct-`rio-vt` migration:
app compilation, the real PTY lifecycle suite, and an Apple Silicon smoke run
are still required for this change. IME, focus reporting, application keypad,
advanced keyboard behaviour, and hyperlinks remain qualification items.
Human visual and input acceptance remains a separate caveat:
the terminal must still be exercised interactively for font metrics, line
spacing, selection, search focus, clipboard behaviour, resize, and shell
lifecycle before a release claim. macOS with Apple's Metal toolchain is the
only runtime-verified platform at present; Linux and Windows are unverified.

## Development

Use the app-local workflow from `apps/cuetty`. The app remains standalone and
does not participate in the root cuenv release pipeline until it is ready:

```bash
nix develop
rustc --version # Must report 1.98.1 for this qualification baseline.
cargo fmt -- --check
cargo check --locked
cargo test --locked
cargo clippy --all-targets --all-features -- -D warnings
cargo build --release --locked
./script/build_and_run.sh --verify
```

The app-local and root flakes both pin Rust **1.98.1**, with a matching
`rust-overlay` revision in each lockfile. Enter the app-local Nix shell before
using Cargo or the packaging script so they use the pinned compiler. This
upgrade supplies the compiler baseline for the direct `rio-vt` qualification
spike and does not change the declared workspace MSRV; it is not evidence that
Rio integration or macOS acceptance has passed. Rust 1.98.1 is the latest stable
release as of 7 September 2026, per the
[official release announcement](https://blog.rust-lang.org/2026/09/03/Rust-1.98.1/).

`build_and_run.sh --verify` uses the installed macOS Metal toolchain to build,
sign, and validate the application bundle. The exact commands and resulting
platform boundary should be reported with every Cuetty implementation change.
