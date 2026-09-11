# Cuetty

Cuetty is a deliberately narrow, usable GPUI terminal host for Cuenv. It is
built around Rio revision `b0694c0707a90dc93fdf01cbf8a658424be285ee`, pinned
from [Rio PR #1927](https://github.com/raphamorim/rio/pull/1927) for lossless
final-output draining and ownership-safe Unix PTY teardown.

The pin is intentional. Upgrade it only with a terminal correctness and
live-interaction validation pass.

```text
Rio PTY + VT state
  -> RioTerminalSession (object-safe TerminalSession boundary)
  -> EventBridge (coalesced damage, lossless ordered controls)
  -> GPUI entity + batched fixed-layout terminal grid
```

Rio is the only PTY and ANSI/VT authority. The host snapshots semantic styled
cells on the GPUI UI thread and resolves them through a replaceable
`TerminalTheme`/`TerminalRenderer` pair. The session trait keeps the frontend replaceable without
leaking `librio` types. GPUI key events become host-owned `TerminalKeyEvent`
values first; only the concrete Rio adapter maps them to Rio's exact
SHIFT/CTRL/ALT/SUPER bit layout. Rio callbacks only touch thread-safe bridge
state.

Rio's negotiated Kitty keyboard flags and xterm `modifyOtherKeys` level are
read from the live session for every keypress. Cuetty encodes the characters,
modifiers, named keys, and repeat events its GPUI event can represent; it no
longer turns a shell's protocol negotiation into a terminal-wide error. Key
release, keypad identity, alternate-key reporting, and IME commit events remain
explicit qualification gaps.

## Run

On macOS with Rust and Apple's Metal Toolchain installed:

```sh
cargo run --release --locked
```

The window starts the default login shell. Click the terminal (mouse-down
focuses it), type a command,
and resize the window. `Command-V` pastes through GPUI's clipboard. OSC 52
clipboard writes are applied on the GPUI thread. Closing the window drops the
surface exactly once and closes the child shell. Wheel or trackpad scrolling
moves through Rio's authoritative history; Shift-Page Up/Down and
Shift-Home/End provide keyboard navigation.

Cuetty normalizes inherited `SHLVL` before spawning the login shell, so a new
window starts as shell level 1 even when the app was launched from a nested
development shell. Subshells increment normally from there.

## Verification

```sh
cargo fmt --check
cargo check --locked
cargo test --locked
cargo clippy --all-targets --all-features -- -D warnings
cargo run --release --locked
```

Manual acceptance is: a visible shell prompt; command typing and output redraw;
Unicode paste; `stty size` matching the resized grid; an OSC 52 clipboard
write; a new tab opening in `$HOME`; and clean shell/window close.

## Visual contract

The shell follows Ghostty's restrained presentation: native traffic lights,
one dark title strip, a compact right-side vertical tab rail, and a full-bleed
Catppuccin Mocha terminal surface. The rail is fixed at a narrow 56px and shows
only tab ordinals; stable path names remain available as tooltips. Restrained
active-tab treatment and two compact footer controls keep the rail readable
without competing with terminal content. The Rio surface sits in an intentional 8px inset below the title strip
beside the rail. The inset uses the same surface colour, so it reads as breathing room
inside one continuous terminal canvas rather than a contrasting frame. Clicking
the terminal viewport requests focus.
Text uses the configured `MonaspiceNe Nerd Font` family with explicit `Noto
Color Emoji`, `Monaspace Neon`, and macOS symbol/monospace fallbacks so prompt
and directory glyphs do not depend on GPUI's default fallback selection.
Terminal text is deliberately 16px with a 1.2 line-height multiplier; that
raw 19.2px line height is snapped once to the shared 20px device-pixel cell
used by both GPUI painting and Rio resizing.
The focused label and cursor follow GPUI's current focus handle, including blur.
Rio
colour semantics (default, indexed, RGB, bold, dim, inverse, and hidden) stay
in `TerminalFrame` until the renderer resolves them through the theme.
The pinned Rio public snapshot exposes cursor position, shape, and visibility.
The renderer retains an explicit focused host-default block policy only when
the adapter reports `Known::Unknown`; it clips positions at render time.
Underlines are host approximations: single and double use distinct
thicknesses, curly is wavy, and dotted/dashed currently collapse to a solid
underline.

The rail footer provides new-tab and Settings entry points. Settings is a
session-only destination with transactional Apply/Cancel/Reset controls for
font size and line height; it never mounts terminal key or mouse handlers. The
native Cuetty menu contains Quit Cuetty and Cmd-Q uses the same application
action. Before calling the POC visually usable, check that native window
controls and the rail remain visible, the prompt fills the terminal surface
without a second frame, the cursor changes with focus, ANSI colour and Unicode
remain correct, and resizing changes the shell's `stty size` without stealing
tab clicks.

## Deliberate limitations

This POC exposes one GPUI terminal surface per tab, with an independent live
Rio session behind each tab. Cmd-T creates a new session; Cmd-W closes only the
active tab/session and closes the window when it is the final tab. Cmd-D and
Cmd-Shift-D report that split panes are unavailable; they never create a fake
terminal pane. Cmd-[ / Cmd-] remain reserved for future pane traversal. Mouse
selection and the current literal visible-frame search hit are painted directly
over the terminal grid; Cmd-C copy and paste are live too. Rio-owned scrollback
is live, but selection and search are cleared when the viewport moves because
their coordinates are still visible-frame-only. It does not yet have live workspace restore,
plugin runtime, Kitty graphics, ligatures, or complete IME/non-Latin
composition. Workspace persistence has a bounded versioned codec and atomic
file store, but live restore and Rio session reconnect are not wired into the
product shell.
The renderer uses one custom GPUI element, merges compatible background spans,
and shapes adjacent compatible cells as fixed-width text batches. Renderer cell
width is measured from the selected font, while line height follows the explicit
16px/1.2 presentation contract; both are snapped once to shared device-pixel
cell dimensions used by painting and Rio resizing. The current Rio public
snapshot adapter preserves backend-declared cluster text, wide-cell occupancy,
and wrapped placeholders. Complete IME, ZWJ shaping, hyperlink, and emoji
presentation correctness is deliberately not claimed.

The built-in colour scheme is Catppuccin Mocha: Crust frames the host, Mantle
frames the rail, Base fills the terminal, and the complete ANSI palette follows
Catppuccin's Mocha mapping.

The terminal render resizes Rio from the pane canvas measured inside GPUI's
actual content panel. The measurement therefore already excludes the title
strip, terminal inset, divider, and fixed rail width instead of trying to
reconstruct those dimensions from the outer window. This keeps window and rail
changes on the same grid, preserves one-cell clamping and resize deduplication,
and prevents a stale wider frame from clipping long output. Startup,
input, resize, paste, and close failures remain visible in the view rather than
being dropped.

## Cuenv presentation

Cuetty reads the `cuetty` CUE package from the active shell's exact current
directory. Any `.cue` file in that directory may declare the package; no
specific filename is required. A directory without that package is neutral.
Standalone packages can evaluate without a module; imports that require a
module context fail through the normal CUE error path.

```cue
package cuetty

banner: "Welcome to Cuetty"
border: "#ff0000"
```

`banner` is a bounded single line and `border` must be a quoted `#RRGGBB`
colour. Rio reports shell CWD changes through its public working-directory API;
Cuetty re-evaluates and rebinds its native directory watcher after `cd`.
Malformed matching configuration retains the last valid presentation and shows
an integration notice. CUE evaluation never runs on the GPUI thread.

On macOS, `xcodebuild -downloadComponent MetalToolchain` installs the Metal
compiler that GPUI needs. Runtime is only verified when
`cargo run --release --locked` successfully opens Cuetty.

## Nix

The app-local flake provides `packages.cuetty`, `apps.cuetty`, and the focused
`cuetty-test`, `cuetty-clippy`, and `cuetty-fmt` checks. Its only inputs are
Nixpkgs, flake-utils, and the Rust overlay.
