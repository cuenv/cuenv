# Cuetty

Cuetty is a deliberately narrow, usable GPUI terminal host for Cuenv. It is
built around the pinned Rio engine revision `b0b79c1ebadc8d6a9a79c4c44a91a42b3ea439d1`.

The pin is intentional: it is the historical Rio frontend revision whose
public surface snapshot Cuetty adapts. Upgrade it only with a terminal
correctness and live-interaction validation pass.

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

## Run

On macOS with Rust and Apple's Metal Toolchain installed:

```sh
cargo run --release --locked
```

The window starts the default login shell. Click the terminal (mouse-down
focuses it), type a command,
and resize the window. `Command-V` pastes through GPUI's clipboard. OSC 52
clipboard writes are applied on the GPUI thread. Closing the window drops the
surface exactly once and closes the child shell.

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
write; and clean shell/window close.

## Visual contract

The shell follows the pinned Cuetty presentation patterns: a compact vertical
tab rail, warm near-black terminal surface, readable fixed-pitch text, and an
explicit focused cursor. The rail is a real resizable panel: drag its edge to
choose a width, or collapse it to a 50px number-only mode and restore the last
expanded width. Tab titles become tooltips in compact mode, so navigation stays
usable without sacrificing context. The Rio surface is full-bleed beside the
rail, and clicking the terminal viewport requests focus.
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
The pinned Rio public snapshot exposes cursor position but not cursor shape or
visibility, so the renderer applies an explicit focused host-default block
policy only when the adapter reports `Known::Unknown`; it clips positions at
render time. Underlines are host approximations: single and double use distinct
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
terminal pane. Cmd-[ / Cmd-] remain reserved for future pane traversal. Mouse selection
and the current literal visible-frame search hit are painted directly over the
terminal grid; Cmd-C copy and paste are live too. It does not yet have live workspace restore,
plugin runtime, Kitty graphics, ligatures, or complete IME/non-Latin
composition. Workspace persistence has a bounded versioned codec and atomic
file store, but live restore and Rio session reconnect are not wired into the
product shell.
The renderer uses one custom GPUI element, merges compatible background spans,
and shapes adjacent compatible cells as fixed-width text batches. Renderer cell
width is measured from the selected font, while line height follows the explicit
16px/1.2 presentation contract; both are snapped once to shared device-pixel
cell dimensions used by painting and Rio resizing. The current Rio public
snapshot adapter still exposes one `char` per visible cell. Wide-cell occupancy
and wrapped placeholders are preserved, but complete combining-mark/ZWJ
cluster, hyperlink, cursor-mode, and emoji presentation correctness is
deliberately not claimed.

The terminal element measures its own post-layout GPUI canvas bounds, not the
outer window, and notifies the entity when those bounds change so the next
render applies the resize to Rio. This preserves one-cell clamping and resize
deduplication. Startup,
input, resize, paste, and close failures remain visible in the view rather than
being dropped.

On macOS, `xcodebuild -downloadComponent MetalToolchain` installs the Metal
compiler that GPUI needs. Runtime is only verified when
`cargo run --release --locked` successfully opens Cuetty.

## Nix

The app-local flake provides `packages.cuetty`, `apps.cuetty`, and the focused
`cuetty-test`, `cuetty-clippy`, and `cuetty-fmt` checks. Its only inputs are
Nixpkgs, flake-utils, and the Rust overlay.
