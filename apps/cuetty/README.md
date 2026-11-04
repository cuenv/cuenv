# Cuetty

Cuetty is a deliberately narrow, usable GPUI terminal host for Cuenv. It is
built around upstream Rio revision `fb582f066a2df7f95055eedb513752adc8c5f6b8`,
which includes [Rio PR #1930](https://github.com/raphamorim/rio/pull/1930) for
final-output draining and ownership-safe Unix PTY teardown.

The pin is intentional. Upgrade it only with a terminal correctness and
live-interaction validation pass. Existing macOS qualification evidence applies
to the previous fork revision and must be repeated for the upstream pin.

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

The window starts the default login shell and requests focus for the terminal.
Type a command and resize the window. `Command-V` pastes through GPUI's
clipboard. OSC 52 clipboard writes are applied on the GPUI thread. Closing the window drops the
surface exactly once and closes the child shell. Wheel and trackpad input
normally moves through Rio's authoritative history. When an application requests
a supported mouse-reporting mode, Cuetty sends it wheel reports; in the
alternate screen with alternate scroll enabled and mouse reporting disabled,
Cuetty sends arrow keys.
Shift-wheel requests local history instead, when history is available in the
current screen. Shift-Page Up/Down and Shift-Home/End provide keyboard
navigation.

Application mouse reporting currently covers the wheel only: clicks, drags,
and pointer motion are not reported to applications, and local text selection
remains available. The original X10 mouse mode does not support wheel reports;
legacy non-SGR reports have coordinate limits. An isolated Linux Xvfb/Niri
smoke test with Codex 0.166.0 exercised wheel-up and wheel-down in a local
transcript and confirmed keyboard focus without an initial click. Physical
device input and macOS still require interactive verification.

Cuetty normalizes inherited `SHLVL` before spawning the login shell, so a new
window starts as shell level 1 even when the app was launched from a nested
development shell. Subshells increment normally from there.

## Verification

The app-local Nix environment includes `libxcb` for Linux linking.

```sh
cargo fmt --check
cargo check --locked
cargo test --locked
cargo clippy --all-targets --all-features -- -D warnings
cargo run --release --locked
```

Manual acceptance is: a visible shell prompt with immediate keyboard focus;
command typing and output redraw; Unicode paste; `stty size` matching the
resized grid; an OSC 52 clipboard
write; shell control chords such as Ctrl-C and Ctrl-U; a new tab opening in
`$HOME`; and clean shell/window close.

## Visual contract

The shell follows Ghostty's restrained presentation: native traffic lights,
one dark title strip, a compact right-side vertical tab rail, and a full-bleed
Catppuccin Mocha terminal surface. The rail is fixed at a narrow 56px and shows
only tab ordinals; stable path names remain available as tooltips. Restrained
active-tab treatment and compact footer controls keep the rail readable
without competing with terminal content. By default, the Rio surface sits in an
8px inset below the title strip beside the rail. The inset uses the same surface
colour, so it reads as breathing room inside one continuous terminal canvas
rather than a contrasting frame. Clicking the terminal viewport requests focus.
Text uses the configured `MonaspiceNe Nerd Font` family with explicit `Noto
Color Emoji`, `Monaspace Neon`, and macOS symbol/monospace fallbacks so prompt
and directory glyphs do not depend on GPUI's default fallback selection.
Terminal text defaults to 16px with a 1.2 line-height multiplier; that
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

Font size, line height, terminal padding, and terminal transparency are configured
through global and per-directory CUE files. Padding is independently configurable
on the top, right, bottom, and left and is applied outside the measured terminal
viewport so Rio receives the actual remaining grid size. Transparency spans 0%
(opaque) through 100% (fully transparent) and changes only the terminal canvas.
The title strip and right-side tab rail retain a fixed dark translucent layer
over native macOS blur, while terminal text, the cursor, tab pills, selection,
and controls remain opaque. The native Cuetty menu contains Quit Cuetty and
Cmd-Q uses the same application action. Before calling the POC visually usable,
check that native window
controls and the rail remain visible, the prompt fills the terminal surface
without a second frame, the cursor changes with focus, ANSI colour and Unicode
remain correct, and resizing changes the shell's `stty size` without stealing
tab clicks.

## Deliberate limitations

This POC exposes one GPUI terminal surface per tab, with an independent live
Rio session behind each tab. Cmd-T creates a new session; Cmd-W closes only the
active tab/session and closes the window when it is the final tab. Shortcuts
shown as Cmd-* use Command on macOS and Ctrl-Shift on Linux (Ctrl-Shift-Alt for
the Cmd-Shift-* forms), so plain Ctrl chords such as Ctrl-C, Ctrl-D, and Ctrl-W
always reach the shell. The task palette is the exception: it defaults to
Cmd-K on macOS and plain Ctrl-K elsewhere, and only takes the chord when the
active directory has a task catalogue, so Ctrl-K stays the shell's kill-line in
ordinary directories. Cmd-D and
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
width is measured from the selected font, while line height follows the configured
font size and line-height multiplier; both are snapped once to shared device-pixel
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

## Cuetty CUE configuration

Cuetty reads the `cuetty` CUE package from the active shell's exact current
directory. Any `.cue` file in that directory may declare the package; no
specific filename is required. A directory without that package is neutral.
Standalone packages can evaluate without a module; imports that require a
module context fail through the normal CUE error path.

```cue
package cuetty

banner: "Welcome to Cuetty"
rail: "#ff0000"
titlebar: false
font: {
    size:       18
    lineHeight: 1.3
}
padding: {
    top:    12
    right:  8
    bottom: 12
    left:   8
}
transparency: 10
```

`banner` is a bounded single line and `rail` must be a quoted `#RRGGBB`
colour that tints the tab rail while that tab is active. Terminal panes are
never framed; the legacy `border` field is rejected. `titlebar: false` hides
the Cuetty title strip while that tab is active so the terminal extends to the
top of the window; it defaults to `true`. With the strip hidden there is no
drag region, and on macOS the traffic lights float over the terminal's top-left
corner. Rio reports shell CWD changes through its public working-directory API;
Cuetty re-evaluates and rebinds its native directory watcher after `cd`.
Malformed matching configuration retains the last valid presentation for that
directory, if one exists, and shows an integration notice. Unknown fields are
rejected.

The appearance fields are optional and apply to the active tab:

| Field | Range | Default |
| --- | --- | --- |
| `font.size` | 1–128 pixels | 16 |
| `font.lineHeight` | 0.5–3 multiplier | 1.2 |
| `padding.top`, `padding.right`, `padding.bottom`, `padding.left` | 0–128 integer pixels per edge | 8 each |
| `transparency` | 0–100 integer percent | 0 |

There is no in-app Settings page; edit the CUE files to change these values.

A global `package cuetty` in `$XDG_CONFIG_HOME/cuetty` (or `~/.config/cuetty`),
in any `.cue` file, is CUE-unified beneath each directory's package, or applies
alone where a directory has none. Write overridable global values as CUE
defaults, for example `rail: *"#89b4fa" | string` and
`padding: top: *8 | int`; a concrete conflict is an invalid configuration.
Each padding edge may be overridden independently. CUE evaluation never runs on
the GPUI thread. The global directory is watched when it exists; if it is
created later while the working directory is known, it is picked up on the
next directory change. When the working directory is unavailable, Cuetty
applies only the global package and checks for its directory on each poll.

`keybindings: palette: "ctrl-shift-p"` rebinds the task palette. Chords are
`-`-separated modifiers (`ctrl`, `alt`, `shift`, `cmd`/`super`) followed by a
key and match that exact modifier set.

Home Manager users can manage the global package through the flake's
`homeManagerModules.cuetty`:

```nix
{
  imports = [ cuenv.homeManagerModules.cuetty ];
  programs.cuetty = {
    enable = true;
    settings = {
      rail = "#89b4fa";
      titlebar = false;
      keybindings.palette = "ctrl-k";
      font = { size = 16.0; lineHeight = 1.2; };
      padding = { top = 8; right = 8; bottom = 8; left = 8; };
      transparency = 0;
    };
  };
}
```

The module writes `$XDG_CONFIG_HOME/cuetty/config.cue`, rendering every
`settings` value as a CUE default so directory packages can still override
it; `extraConfig` appends raw CUE. `package` defaults to the flake's Cuetty
build and may be set to `null` to manage configuration only.

When the same directory contains an evaluable `package cuenv`, Cuetty also
discovers its `tasks` map. The task button in the fixed right rail opens a
floating, non-resizing task sidebar; Cmd-K (Ctrl-K off macOS, configurable
through `keybindings.palette`) opens a searchable command palette. Selecting a task invokes the existing CLI in the same terminal pane
whose working directory supplied the catalogue. The
`cuenv task --path … --package cuenv …` CLI path preserves its graph, cache,
hook, secret-redaction, cancellation, and exit behaviour. Tasks with required
parameters are staged at the prompt for editing instead of running
incompletely. Directories without a task catalogue remain ordinary terminals.

On macOS, `xcodebuild -downloadComponent MetalToolchain` installs the Metal
compiler that GPUI needs. Runtime is only verified when
`cargo run --release --locked` successfully opens Cuetty.

The terminal content panel establishes its own full-size flex layout so the
dark terminal surface is present on the first window frame; launch must not
depend on a later resize to replace AppKit's default backing colour.

Wheel and trackpad deltas retain AppKit's system-adjusted direction, including
the macOS Natural Scrolling preference, while sub-line motion is accumulated
per terminal pane before the event is routed to the application or history.

## Nix

From the repository root, use `nix build .#cuetty`, `nix run .#cuetty`, or
`nix develop .#cuetty`. The root flake also provides the focused `cuetty-test`,
`cuetty-clippy`, and `cuetty-fmt` checks.
On Linux, the packaged executable provides GPUI's runtime library search paths,
including the Wayland client library loaded dynamically at startup, without
adding them to the `LD_LIBRARY_PATH` inherited by terminal shells.
