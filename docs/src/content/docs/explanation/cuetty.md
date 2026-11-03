---
title: Cuetty
description: Cuetty terminal app architecture and development workflow
---

Cuetty is the experimental cuenv terminal app in `apps/cuetty`. It is a
standalone Rust desktop application using GPUI for the product shell and the
pinned Rio terminal engine for terminal state, parsing, PTY integration, and
semantic render frames.

The direct-`rio-vt` revision is:

`fb582f066a2df7f95055eedb513752adc8c5f6b8`

That pin is intentional. It includes
[Rio PR #1930](https://github.com/raphamorim/rio/pull/1930), which preserves
final PTY output under terminal-lock contention and gives Unix child teardown
an ownership-safe shutdown/reap contract. Cuetty uses public APIs available at
this revision rather than private renderer or embedding APIs.
The recorded macOS qualification applies to the previous fork revision;
qualification of this upstream pin remains pending.

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

- A login shell attached to a Rio-backed terminal session. Cuetty resets the
  inherited `SHLVL` base so a new window starts at shell level 1 rather than
  leaking the launcher's nesting into the prompt.
- Fixed-width, fixed-row-height GPUI rendering with semantic colours and
  explicit wide-cell and soft-wrap occupancy.
- Catppuccin Mocha is the built-in terminal colour scheme shared by Rio's
  semantic resolver and the GPUI chrome.
- Ghostty is the visual reference for restrained native chrome: traffic lights,
  one title strip, stable single-line tab labels, and a full-bleed terminal.
- Keyboard input, including macOS Control chords, paste, resize, title updates, clipboard copy, mouse
  selection, Rio-owned history navigation, and visible-frame literal search.
- The active pane's post-layout GPUI canvas is the source of truth for each Rio
  resize, so the measured width already excludes the title strip, terminal
  inset, divider, and fixed rail width instead of leaving a stale frame
  clipped in the viewport.
- The terminal surface keeps an 8px inset from the host chrome; the inset uses
  the same terminal surface colour and is subtracted from the Rio grid
  dimensions as well as the rendered bounds. Its opposing edges constrain the
  surface size so the last row remains inside the content panel.
- Independent live Rio sessions for each tab with a replaceable workspace
  model. Cmd-T creates a session-backed tab and Cmd-W closes the active one;
  every newly-created session starts in `$HOME`,
  split shortcuts are rejected with a visible notice until pane/session
  allocation is implemented, and the UI never presents a fake terminal pane.
  Application shortcuts written as Cmd-* use Command on macOS and Ctrl-Shift
  on Linux, so plain Ctrl chords such as Ctrl-C always reach the shell. The
  task palette is the one configurable exception (see below).
- A fixed 56px right-side vertical tab rail. It shows tab ordinals with stable
  path-name tooltips and deliberately has no resize or expand mode.
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
single-line `banner` and a quoted `rail: "#RRGGBB"`. The banner supplies the
tab tooltip; for example, `rail: "#ff0000"` tints the tab rail while that tab
is active. Terminal panes are never framed, and the former `border` field is
rejected as unknown. `titlebar: false` hides the Cuetty title strip while that
tab is active and lets the terminal extend to the top of the window; it
defaults to `true`. `keybindings: palette:` rebinds the task palette; see
[Keybindings](#keybindings). A hidden strip removes the window drag region, and on macOS
the traffic lights float over the terminal's top-left corner.

The cuenv repository includes a root `cuetty.cue` with a `cuenv` banner and a
Catppuccin Mocha blue rail. It applies while the shell is in the repository
root; subdirectories can provide their own `package cuetty` configuration.
When using the CUE CLI at the root, select the package explicitly, for example
`cue export .:cuetty` or `cue export .:cuenv`.

### Global configuration

Cuetty also evaluates package `cuetty` in `$XDG_CONFIG_HOME/cuetty`, falling
back to `~/.config/cuetty` when `XDG_CONFIG_HOME` is unset or relative. As with
directory configuration, any `.cue` filename may provide the package; there is
no fixed `config.cue`. The global package is CUE-unified with the active
directory's package by the evaluator (`cuengine`'s `unify` option), not merged
by Cuetty. Unification means a concrete global value conflicts with a
different directory value, so global values meant to be overridden are written
as defaults:

```cue
package cuetty

rail:     *"#89b4fa" | string
titlebar: *false | bool
```

A directory without package `cuetty` uses the global package alone. A
conflict or an invalid global package is reported like any invalid matching
configuration and keeps the last good presentation. The global directory is
watched alongside the active directory when it exists; one created later is
picked up on the next directory change.

#### Home Manager

The root flake exports `homeManagerModules.cuetty` (also `default`), which
installs Cuetty and writes the global package to
`$XDG_CONFIG_HOME/cuetty/config.cue`:

```nix
{
  imports = [ cuenv.homeManagerModules.cuetty ];
  programs.cuetty = {
    enable = true;
    settings = {
      rail = "#89b4fa";
      titlebar = false;
      keybindings.palette = "ctrl-shift-p";
    };
    extraConfig = ''
      banner: *"home" | string
    '';
  };
}
```

Every `settings` scalar is rendered as a CUE default (`*value | type`), so the
generated package never conflicts with a directory that sets its own value.
`extraConfig` is appended verbatim and is responsible for its own defaults.
Set `package = null` to manage only the configuration.

Directory and module-metadata changes are watched natively. Evaluation and
validation occur off the GPUI thread. Invalid matching CUE keeps the last
valid presentation for that directory and exposes an integration notice.

Cuetty separately evaluates `package cuenv` in the same exact directory and
surfaces its task, group, and sequence names in a floating sidebar and a
command palette. These overlays do not participate in terminal layout, so
opening them does not change the PTY's reported rows or columns. Launching a
task delegates to `cuenv task` in the same pane that supplied the catalogue;
required task parameters leave an editable command at the prompt. The app does not duplicate
the task graph, cache, hooks, or secret resolution.

### Keybindings

The task palette opens with Cmd-K on macOS and plain Ctrl-K elsewhere. Rebind
it from any `package cuetty`, global or per-directory:

```cue
package cuetty

keybindings: palette: *"ctrl-shift-p" | string
```

A chord is `-`-separated modifiers (`ctrl`, `alt`, `shift`, `cmd`/`super`)
followed by a key, matched case-insensitively against that exact modifier set.
The palette only claims its chord when the active directory has a task
catalogue; elsewhere the key reaches the shell, so the Ctrl-K default does not
cost kill-line in ordinary directories. An unparseable chord or any other
`keybindings` field is an invalid configuration.

## Current limits

The following are deliberately documented as staged work rather than implied
support:

- Full-history selection and search; history navigation is live, but current
  selection/search coordinates cover one visible viewport and search is
  literal-only.
- Complete Unicode behaviour, including combining marks, ZWJ sequences,
  emoji presentation, and IME correctness.
- Kitty graphics and other image protocols.
- tmux, remote, attach, and reconnect backends.
- Rich task progress, cancellation controls, and source-linked diagnostics;
  the current task surface launches the canonical CLI in a terminal tab.
- Wasm plugins and a plugin/component-tree runtime.
- Split-pane session allocation, plus live workspace restore/reconnect.

Earlier implementation work recorded focused Rust validation and macOS bundle
verification. Those results do not validate the new direct-`rio-vt` migration:
app compilation, the real PTY lifecycle suite, and an Apple Silicon smoke run
are still required for this change. The GPUI-representable Kitty and
`modifyOtherKeys` subset now has deterministic coverage; IME, focus reporting,
application keypad identity, key releases, alternate-key reporting, and
hyperlinks remain qualification items.
Human visual and input acceptance remains a separate caveat:
the terminal must still be exercised interactively for font metrics, line
spacing, selection, search focus, clipboard behaviour, resize, and shell
lifecycle before a release claim. macOS with Apple's Metal toolchain is the
only runtime-verified platform at present; Linux and Windows are unverified.

## Development

From the repository root, enter Cuetty's development shell and work in its
standalone Cargo workspace. It does not participate in the root cuenv release
pipeline until it is ready:

```bash
nix develop .#cuetty
cd apps/cuetty
rustc --version # Must report 1.98.1 for this qualification baseline.
cargo fmt -- --check
cargo check --locked
cargo test --locked
cargo clippy --all-targets --all-features -- -D warnings
cargo build --release --locked
./script/build_and_run.sh --verify
```

The root flake pins Rust **1.98.1**. Enter the Cuetty Nix shell before
using Cargo or the packaging script so they use the pinned compiler. This
upgrade supplies the compiler baseline for the direct `rio-vt` qualification
spike and does not change the declared workspace MSRV; it is not evidence that
Rio integration or macOS acceptance has passed. Rust 1.98.1 is the latest stable
release as of 7 September 2026, per the
[official release announcement](https://blog.rust-lang.org/2026/09/03/Rust-1.98.1/).

`build_and_run.sh --verify` uses the installed macOS Metal toolchain to build,
sign, and validate the application bundle. The exact commands and resulting
platform boundary should be reported with every Cuetty implementation change.

On Linux, the root flake's Cuetty executable includes the search paths GPUI
needs for dynamically loaded libraries, including the Wayland client. These
paths are not added to `LD_LIBRARY_PATH`, so terminal shells inherit the caller's
value without Cuetty's libraries overriding their own programs. The package
uses the root flake's NixOS unstable input so its C library can load graphics
drivers built against recent NixOS releases.
