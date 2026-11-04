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
one title strip that shares the terminal's surface and names the active tab, which
also titles the window for window switchers and compositors, a
right-side tab sidebar floating inset from the window's edges so it sits comfortably
with rounded window corners, and a full-bleed Catppuccin Mocha terminal surface. The sidebar is 200px wide and lists tabs by name. Its toggle, or
Cmd-B (Ctrl-Shift-B on Linux), collapses it to 56px with an initial for each tab, taken
from its user-set name, project banner, or directory so it stays put as programs come
and go, and the full name as a tooltip; `sidebar: "collapsed"` in CUE starts it that
way. A window opens with the global package's look and sidebar when that evaluates
within half a second, so its first frame is already the right size. Only the first
configuration a window evaluates settles the sidebar, so neither `cd` nor a later
edit moves it, and the toggle outranks that setting for the life of the
window. Names resolve in
`src/terminal/tab_label.rs`: a user-set name, then a running Cuenv task, then the
foreground program or a title it set, then the directory, then `Terminal`. A title
lasts only as long as the program it was set under, one that names a directory stands
in only while the working directory is unknown, a foreground that cannot be named
keeps what was last known (a pipeline whose first command has exited, as in
`git log | less`, cannot be), a shell running a script is named after the script, and
Nix's `.name-wrapped`
binaries are shown as `name`. A tab
list taller than the window scrolls, and follows the active tab. Restrained
active-tab treatment and compact footer controls keep the sidebar readable
without competing with terminal content. By default, the Rio surface sits in a
14px inset below the title strip beside the sidebar. The inset, and any space left
over past the grid's last whole column and row, use the same surface colour from
one background layer, so they read as breathing room inside one continuous
terminal canvas rather than a contrasting frame or strip. Clicking the terminal viewport requests focus.
Text defaults to the `MonaspiceNe Nerd Font` family with explicit `Noto
Color Emoji`, `Monaspace Neon`, and macOS symbol/monospace fallbacks so prompt
and directory glyphs do not depend on GPUI's default fallback selection.
Terminal text defaults to 16px with a 1.2 line-height multiplier; that
raw 19.2px line height is snapped once, with the font's advance, to whole device
pixels: 19px at 1x and 38 device pixels at 2x, so columns and rows land on device
pixels at any scale. GPUI painting and Rio resizing share that one cell, and the
sizes the PTY reports are in device pixels.
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

Font family, font size, line height, terminal padding, and terminal transparency are configured
through global and per-directory CUE files. Padding is independently configurable
on the top, right, bottom, and left and is applied outside the measured terminal
viewport so Rio receives the actual remaining grid size. Transparency spans 0%
(opaque) through 100% (fully transparent). The terminal canvas, title strip, and
right-side sidebar share that one opacity from a single layer, so the window reads
as one material:
at 0% nothing shows through and the compositor treats the window as opaque, and
above 0% everything sits over native blur together, including the sidebar's outline
and its hover and active-tab fills, which stop fading at 50% opacity so a very
transparent window still shows the panel and gives feedback. The sidebar has no fill
of its own above 0%, because a second layer would be more opaque than the rest. Terminal text,
the cursor, selection, chrome text, and the active tab's marker remain opaque, as
do the floating task panels and their outlines, whose own text would otherwise
compete with the terminal text they cover. The native Cuetty menu contains Quit Cuetty and
Cmd-Q uses the same application action. Before calling the POC visually usable,
check that native window
controls and the sidebar remain visible, the prompt fills the terminal surface
without a second frame, the cursor changes with focus, ANSI colour and Unicode
remain correct, and resizing changes the shell's `stty size` without stealing
tab clicks.

## Deliberate limitations

This POC exposes one GPUI terminal surface per tab, with an independent live
Rio session behind each tab. Cmd-T creates a new session; Cmd-W closes only the
active tab/session and closes the window when it is the final tab. Shortcuts
shown as Cmd-* use Command on macOS and Ctrl-Shift on Linux (Ctrl-Shift-Alt for
the Cmd-Shift-* forms), so plain Ctrl chords such as Ctrl-C, Ctrl-D, and Ctrl-W
reach the shell. Ctrl-Page Up and Ctrl-Page Down select the previous or
next tab, wrapping at either end. Cmd-1 to Cmd-8 on macOS and Ctrl-1 to Ctrl-8
elsewhere select a tab by position, and 9 selects the last. Those Ctrl chords are the
one plain set Cuetty takes from applications, including the legacy aliases Ctrl-2 to
Ctrl-8 for NUL, ESC, FS, GS, RS, US, and DEL, but only when that tab exists; with no
such tab the key reaches the terminal, and a palette chord on a digit wins. Holding
the modifier for a moment shows the numbers as faint hints. The task palette defaults to
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
product shell. Schema 2 records each tab's optional user-set name; schema 1
snapshots still decode, with no name.
The renderer uses one custom GPUI element and merges compatible background spans.
Adjacent cells sharing a font are shaped together, even across colours, so a font's
contextual alternates see the surrounding text. Every glyph is then placed from the
column its source text belongs to, never from the font's advances, so `calt`,
combining marks, and fallback fonts cannot shift a column, and all rows share one
baseline taken from the configured font. Dim text blends halfway towards the
background it is drawn on, so it reads as quieter on a light theme as well as a dark
one, and concealed text paints no glyph at all. Renderer cell
width is measured from the selected font, while line height follows the configured
font size and line-height multiplier; both are rounded once to whole device pixels
at the window's scale factor, and painting, hit testing, and Rio resizing all use
that cell. The surface is measured in device pixels too, so the grid the PTY is
told about is exactly the one painted, and moving a window between displays
resizes it. The current Rio public
snapshot adapter preserves backend-declared cluster text, wide-cell occupancy,
and wrapped placeholders. Box drawing (U+2500–257F, including rounded corners and diagonals), block elements
(U+2580–259F, including shades), and the Powerline separators (U+E0B0–E0B3) are
drawn as shapes on the cell's device pixels rather than through the font, so rules
and blocks meet with no seam at any line height. They keep the cell's foreground and
behave like text under a selection or a block cursor. Complete IME, ZWJ shaping, hyperlink, and emoji
presentation correctness is deliberately not claimed.

The built-in colour scheme is Catppuccin Mocha: Base fills the window, Mantle sets
the sidebar off from it when the window is opaque, Surface 0 draws faint dividers, Surface 1 tints
selections, Yellow marks the current search match with Crust text on it, Blue is
the accent, and the
complete ANSI palette follows Catppuccin's Mocha mapping. Every one of these,
plus muted text and the raised and hover surfaces used by floating panels, lives in
`ThemeColors`; none is hard-coded by a renderer.

All chrome spacing, radii, type sizes, border widths, elevation, and colours come
from one token set in `src/terminal/chrome/tokens.rs`; chrome views, tooltips
included, do not write their own literals.

Chrome text uses the platform's sans-serif UI font on a small type scale, and only
paths and shortcuts use the terminal's monospaced family. GPUI's UI font name is only
meaningful on macOS and Windows: on Linux it means IBM Plex Sans and otherwise a
fallback stack led by a monospaced font, so Cuetty picks the first installed family
from a short list of desktop UI fonts, starting with Inter.

Chrome controls draw from one icon set: a few [Lucide](https://lucide.dev) glyphs
in `assets/icons`, compiled into the binary and served through GPUI's asset source,
at a slightly lighter stroke than upstream to sit beside the text. Lucide's ISC
licence is alongside them. Add a glyph there rather than using a text character.

The terminal render resizes Rio from the pane canvas measured inside GPUI's
actual content panel. The measurement therefore already excludes the title
strip, terminal inset, and the sidebar's current width instead of trying to
reconstruct those dimensions from the outer window. This keeps window and sidebar
changes on the same grid, preserves one-cell clamping and resize deduplication,
and prevents a stale wider frame from clipping long output. A measurement taken while
GPUI draws asks for a new frame once that draw has finished, because invalidation
during a draw is ignored, so the grid follows within a few frames even at an idle
prompt. Startup,
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
    family:     "Monaspace Neon"
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
project accent. It colours the small marker on the active tab while that tab is
active, in place of the window's accent, and never fills the sidebar. `accent`, also
a quoted `#RRGGBB`, is that window accent, for instance to match a compositor's
window border: the marker uses `rail`, then `accent`, then the theme's accent, and
the notice marker and palette caret use `accent`, then the theme's. Terminal panes are
never framed; the legacy `border` field is rejected. `titlebar: false` hides
the Cuetty title strip while that tab is active so the terminal extends to the
top of the window; it defaults to `true`. Off macOS the strip draws only under
client-side decorations: Cuetty asks for server-side ones, and a window that gets them
leaves its title and moving it to the windowing system, whether or not that shows a
title bar, so `titlebar` has no effect there and the sidebar names the tab. With the strip hidden the small gap above the
sidebar moves the window. GPUI cannot hide the traffic lights once a window exists, so on
macOS the terminal starts below them instead, in a band the strip's height that also moves
the window; a larger `padding.top` still applies. Rio reports shell CWD changes through its public working-directory API;
Cuetty re-evaluates and rebinds its native directory watcher after `cd`. Until that
evaluation lands, a tab keeps its look but drops the previous `banner` and `rail`, and
a new tab starts from the look of the tab it opened from, so neither a `cd` nor a new
tab flashes the defaults or resizes the grid.
Malformed matching configuration retains the last valid presentation for that
directory, if one exists, and shows an integration notice: a small accent marker
beside the sidebar's controls, with the message in its tooltip. The tooltip stops
at twelve lines; `cue eval .:cuetty` in that directory prints a whole evaluation
error. Workspace action failures appear the same way until the next tab switch or
successful action. So does a task pasted without running, on its own pane, until Enter
or Ctrl-C finishes the line or the foreground changes; a configuration error there
takes priority. Unknown fields are rejected.

The appearance fields are optional and apply to the active tab:

| Field | Range | Default |
| --- | --- | --- |
| `font.family` | Nonempty font family name | `MonaspiceNe Nerd Font` |
| `font.size` | 1–128 pixels | 16 |
| `font.lineHeight` | 0.5–3 multiplier | 1.2 |
| `font.calt` | Boolean: the font's contextual alternates, such as Monaspace's texture healing. Ligatures stay off either way | `true` |
| `padding.top`, `padding.right`, `padding.bottom`, `padding.left` | 0–128 integer pixels per edge | 14 each |
| `transparency` | 0–100 integer percent | 0 |

There is no in-app Settings page; edit the CUE files to change these values.

A global `package cuetty` in `$XDG_CONFIG_HOME/cuetty` (or `~/.config/cuetty`),
in any `.cue` file, is CUE-unified beneath each directory's package, or applies
alone where a directory has none. Write overridable global values as CUE
defaults, for example `rail: *"#89b4fa" | string` and
`padding: top: *14 | int`; a concrete conflict is an invalid configuration.
Each padding edge may be overridden independently. CUE evaluation never runs on
the GPUI thread. The global directory is watched when it exists; if it is
created later while the working directory is known, it is picked up on the
next directory change. When the working directory is unavailable, Cuetty
applies only the global package and checks for its directory on each poll.

`keybindings: palette: "ctrl-shift-p"` rebinds the task palette. Chords are
`-`-separated modifiers (`ctrl`, `alt`, `shift`, `cmd`/`super`) followed by a
key and match that exact modifier set. A palette chord takes precedence over the tab
number and Ctrl-Page chords it conflicts with, as the binding the user chose.
Task hints show the active tab's chord in the platform's own spelling.

Home Manager users can link their own CUE file as the global package:

```nix
{
  xdg.configFile."cuetty/config.cue".source = ./cuetty.cue;
}
```

The file should declare `package cuetty` and use defaults for any values that
per-directory packages should override.

When the same directory contains an evaluable `package cuenv`, Cuetty also
discovers its `tasks` map. The task button in the sidebar's footer opens a
floating, non-resizing task sidebar; Cmd-K (Ctrl-K off macOS, configurable
through `keybindings.palette`) opens a searchable command palette. Selecting a task invokes the existing CLI in the same terminal pane
whose working directory supplied the catalogue, pasting the command and pressing Enter
separately, because a shell in bracketed paste mode takes a pasted newline as text. Enter
is pressed only while the foreground is an interactive shell, judged from the process's
name and start-up arguments, so a program or a shell running a script (`bash deploy.sh`)
cannot be made to submit something the user was still writing or to answer a script's
question. A shell's builtin, such as `read`, or a subshell looks the same as its prompt
from outside, so a command launched during one does get an Enter. The
`cuenv task --path … --package cuenv …` CLI path preserves its graph, cache,
hook, secret-redaction, cancellation, and exit behaviour. Tasks with required
parameters are staged at the prompt for editing instead of running
incompletely. Both panels take pointer input for themselves rather than passing
it to the terminal beneath, and clicking outside the palette closes it. The
sidebar lists every task and scrolls. The palette shows up to nine matches and sizes itself to them; in a short window its
results scroll under a fixed field and hint, following the keyboard selection. Directories without a task catalogue remain ordinary terminals.

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
