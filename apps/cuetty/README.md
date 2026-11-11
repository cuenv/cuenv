# Cuetty

Cuetty is a deliberately narrow, usable GPUI terminal host for Cuenv. It is
built around Rio revision `200e78994355481c7e06cd840fbcaeb8a1e6b519` of the
[rawkode/rio](https://github.com/rawkode/rio) fork. That is upstream
`fb582f066a2df7f95055eedb513752adc8c5f6b8`, which includes
[Rio PR #1930](https://github.com/raphamorim/rio/pull/1930) for final-output
draining and ownership-safe Unix PTY teardown, plus one `rio-vt` commit that
keeps the OSC 133 marks a shell leaves around a command's output and its exit
status. Return to an upstream revision once that commit is merged there.

The pin is intentional. Upgrade it only with a terminal correctness and
live-interaction validation pass. Existing macOS qualification evidence applies
to an earlier fork revision and must be repeated for this pin.

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
the current tab's directory; and clean shell/window close.

## Visual contract

The shell follows Ghostty's restrained presentation: native traffic lights,
one title strip that shares the terminal's surface, names the active tab, which
also titles the window for window switchers and compositors, and signs itself with a
small muted `cuetty` wordmark at its far end, and a full-bleed terminal surface in
the chosen theme. The terminal keeps the whole width of the window: the chrome floats
over it. Tabs are a column of dots centred on the right edge, the active one a ring
in the marker colour and the rest coloured by status (red for a failure, the accent
for a bell or unread output). Bringing the pointer within 24px of the column opens
it into a 220px list of tab names, which closes again when the pointer leaves; number
hints, a name being edited, and a tab being dragged also hold it open. The project
in the active tab's directory is a pill at the top right, in the title strip when
there is one and in place of the wordmark, naming the project and how it runs, with
an accent dot when there is a notice. Hovering it opens the project panel beneath it.
The panel's pin button, or Cmd-B (Ctrl-Shift-B on Linux), pins the project to a
240px sidebar instead, which the grid makes room for, resizing once rather than on
every frame; `sidebar: "pinned"` in CUE starts it that way, and `"floating"` is the
default. The earlier `"expanded"` and `"collapsed"` still parse, as pinned and
floating. A window opens with the global package's look and sidebar when that evaluates
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
list taller than the window scrolls, and follows the active tab. The terminal fades back slightly while its window does not have the keyboard; inactive
panes have nothing to dim until split panes exist. The foot of the project panel names the command palette's chord.
Dragging a tab onto another moves it to that tab's place in the list. Hovering a tab in the open list reveals a button that closes
it. Double-clicking a tab, or Cmd-R (Ctrl-Shift-R on Linux), edits its name in place: Enter
or clicking elsewhere keeps it, Escape abandons it, and an empty name hands the tab back to
its derived one. Each expanded tab has a muted
second line such as `cuenv · main`, with its directory, unless the tab is already named after
it, and the branch that directory is on. `src/terminal/git_branch.rs` reads that from the
repository's `HEAD` file every couple of seconds without spawning a process; with neither
known the line is absent. `src/terminal/tab_status.rs`
works out each tab's indicator: a bell for a bell rung while the tab was not on screen,
which it never is while the window is in the background, a
ring while a task launched from Cuetty runs, counted from the Enter that runs it if it
needed arguments, a flag once it hands the foreground back, which says it ended and
not how,
and a dot for text that changed while the tab was not on screen. Each has its own shape
and its meaning in the tab's tooltip. Arriving at a tab, or back at the window with it on
screen, clears what was waiting there,
and typing moves on from a finished task. A shell that exits with a failing status or
dies on a signal keeps its tab, with a warning triangle and its final output still
readable above a strip saying how it ended, until Enter, Ctrl-D, or the close chord
dismisses it, and it offers no tasks in the meantime; one that exits cleanly
closes its tab at once. A task launched from Cuetty cannot report failure yet: it
runs in the shell, and Cuetty does not read the status shells report through OSC 133,
which Rio now keeps. Restrained
active-tab treatment and floating chrome that keeps to the window's edges keep the
tabs readable without competing with terminal content. By default, the Rio surface sits in a
14px inset below the title strip. The inset, and any space left
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
The focused label follows GPUI's current focus handle, including blur. The cursor
blinks only if the application asks, with `cursor.blink` to override that: it fades
rather than toggles, stays solid for a moment after output, a key press, or a
switch of tab, and settles solid after about half a minute of nothing happening, so an idle window
stops drawing. Rio raises no event for a program that only shows or hides its
cursor, so the half-second poll notices that instead. Unfocused, or with another window active, it is a hollow outline. On a wide
character it covers both columns, whichever half it landed on.
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
controls and the tab dots remain visible, the prompt fills the terminal surface
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
the modifier for a moment shows the numbers as faint hints. The command palette defaults to
Cmd-K on macOS and Ctrl-Shift-K elsewhere, leaving plain Ctrl-K to the shell.
It is available in every directory. Cmd-D and
Cmd-Shift-D report that split panes are unavailable; they never create a fake
terminal pane. Cmd-[ / Cmd-] remain reserved for future pane traversal. Mouse
selection and every literal visible-frame search hit, opened with Cmd-F (Ctrl-Shift-F on
Linux) into a search bar at the terminal's top right, are painted directly
over the terminal grid, with the current hit picked out from the rest. Releasing a mouse
selection copies it to the system clipboard; a click without a selection leaves the clipboard
alone. Hold a drag at the top or bottom row to scroll through Rio's history and extend the
selection. Cmd-C can copy the selected text again. Rio-owned scrollback also has a thin overlay
scrollbar that shows while the view moves or the pointer comes to the padding at the terminal's
edge, fades away, and can be dragged. An open search stays open and searches what has come into
view. It does not yet have live workspace restore,
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
background it is drawn on, a selection included, so it reads as quieter on a light theme as well as a dark
one, and concealed text paints no glyph at all. Renderer cell
width is measured from the selected font, while line height follows the configured
font size and line-height multiplier; both are rounded once to whole device pixels
at the window's scale factor, and painting, hit testing, and Rio resizing all use
that cell. The surface is measured in logical pixels and turned into device pixels
at each draw's own scale, so the grid the PTY is told about is exactly the one
painted, and moving a window between displays keeps the grid it had. The current Rio public
snapshot adapter preserves backend-declared cluster text, wide-cell occupancy,
and wrapped placeholders. Box drawing (U+2500–257F, including rounded corners and diagonals), block elements
(U+2580–259F, including shades), and the Powerline separators (U+E0B0–E0B3) are
drawn as shapes on the cell's device pixels rather than through the font, so rules
and blocks meet with no seam at any line height. They keep the cell's foreground and
behave like text under a selection or a block cursor. Complete IME, ZWJ shaping, hyperlink, and emoji
presentation correctness is deliberately not claimed.

`theme` in CUE picks one of four complete palettes in `src/terminal/config.rs`:
`catppuccin-mocha` (the default), `catppuccin-latte`, `graphite`, and `paper`. `auto`
uses Latte or Mocha to match the window's appearance and switches live as the desktop
does; on Linux that comes from the XDG desktop portal, and no stated preference counts
as light. Applications asking about colours (OSC 4, 10, 11, and 12) are told the palette
their tab paints in, and a shell starts already knowing it, so Neovim and `bat` pick a
light scheme on a light theme. A test
holds all of them to one contrast contract, so a new token or theme cannot quietly
ship text that does not read. On the default, Base fills the window, Mantle sets
the sidebar off from it when the window is opaque, Surface 0 draws faint dividers, a tint just short
of Surface 1 marks selections so ANSI colours still read on them, Yellow marks the current search match with Crust text on it and a dark Yellow tints the others, Blue is
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

The app's own icon is `assets/app/cuetty.svg`, an original mark: a lowercase c, for cue,
waiting at a block cursor, for tty. The Nix package installs it as the `cuetty` icon in
the hicolor theme and the desktop entry names it. The macOS development bundle has no
icon yet: that takes an `.icns` built on a Mac.

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
theme: "graphite"
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
project accent. It colours the small marker on the active tab, which slides to whichever tab becomes active, while that tab is
active, in place of the window's accent, and never fills the sidebar. `accent`, also
a quoted `#RRGGBB`, is that window accent, for instance to match a compositor's
window border: the marker uses `rail`, then `accent`, then the theme's accent, and
the notice marker and palette caret use `accent`, then the theme's. Both are held to
3:1 against the sidebar, so a colour chosen for one theme still shows on another: one
that already shows is left exactly as given. Terminal panes are
never framed; the legacy `border` field is rejected. `titlebar: false` hides
the Cuetty title strip while that tab is active so the terminal extends to the
top of the window; it defaults to `true`. Off macOS the strip draws only under
client-side decorations: Cuetty asks for server-side ones, and a window that gets them
leaves its title and moving it to the windowing system, whether or not that shows a
title bar, so `titlebar` has no effect there and the tab list names the tab. With the strip hidden the small gap above a
pinned sidebar moves the window. GPUI cannot hide the traffic lights once a window exists, so on
macOS the terminal starts below them instead, in a band the strip's height that also moves
the window; a larger `padding.top` still applies. Rio reports shell CWD changes through its public working-directory API;
Cuetty re-evaluates and rebinds its native directory watcher after `cd`. Until that
evaluation lands, a tab keeps its look but drops the previous `banner` and `rail`, and
a new tab starts from the look of the tab it opened from, so neither a `cd` nor a new
tab flashes the defaults or resizes the grid.
Malformed matching configuration retains the last valid presentation for that
directory, if one exists, and shows an integration notice: an accent dot on the
project pill, and a marker beside the project panel's controls with the message in its tooltip. The tooltip stops
at twelve lines; `cue eval .:cuetty` in that directory prints a whole evaluation
error. Workspace action failures appear the same way until the next tab switch or
successful action. So does a task pasted without running, on its own pane, until Enter
or Ctrl-C finishes the line or the foreground changes; a configuration error there
takes priority. Unknown fields are rejected.

A tab whose session breaks shows what went wrong with Restart shell and Close tab in place of
its terminal, and a window with no terminal because no shell would start says why and offers
New tab. Enter takes the first of these.

The appearance fields are optional and apply to the active tab:

| Field | Range | Default |
| --- | --- | --- |
| `font.family` | Nonempty font family name | `MonaspiceNe Nerd Font` |
| `font.size` | 1–128 pixels | 16 |
| `font.lineHeight` | 0.5–3 multiplier | 1.2 |
| `font.calt` | Boolean: the font's contextual alternates, such as Monaspace's texture healing. Ligatures stay off either way | `true` |
| `font.boldWeight` | 100–900 integer; a family without that exact weight uses its nearest | 600 |
| `padding.top`, `padding.right`, `padding.bottom`, `padding.left` | 0–128 integer pixels per edge | 14 each |
| `cursor.blink` | `"terminal"` to blink as the running application asks, `"on"`, or `"off"` | `"terminal"` |
| `cursor.trail` | Boolean: a short fading trail behind a cursor that jumps. Typing leaves none | `false` |
| `motion` | `"system"` to follow the platform's reduced-motion setting where one can be read, `"full"`, or `"reduced"` | `"system"` |
| `minimumContrast` | 1–21 contrast ratio text is kept to against its background. 1 is off | 1 |
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

`keybindings: palette: "ctrl-shift-p"` rebinds the command palette. Chords are
`-`-separated modifiers (`ctrl`, `alt`, `shift`, `cmd`/`super`) followed by a
key and match that exact modifier set. A palette chord takes precedence over the tab
number and Ctrl-Page chords it conflicts with, as the binding the user chose.
The project panel's hint shows the active tab's chord in the platform's own spelling.

Home Manager users can link their own CUE file as the global package:

```nix
{
  xdg.configFile."cuetty/config.cue".source = ./cuetty.cue;
}
```

The file should declare `package cuetty` and use defaults for any values that
per-directory packages should override.

When the same directory contains an evaluable `package cuenv`, Cuetty also
discovers its `tasks` map. The command palette opens with Cuetty actions such as
new tab, rename tab, and find on screen. Type `$` to search the active tab's
Cuenv tasks, or use the task button in the project panel to open that mode directly.
Deleting `$` returns to commands. Selecting a task invokes the existing CLI in the same terminal pane
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
incompletely. The palette takes pointer input rather than passing it to the
terminal beneath, and clicking outside closes it. Its results scroll under a
fixed field and hint, following the keyboard selection, so every match remains
reachable even when there are more than nine.

**Copy last command output** puts what the last command printed on the clipboard,
without its prompt or command line: all of it for a finished command, what there is so
far for a running one. Rows that have scrolled into history are included and
soft-wrapped rows are joined, as selecting them would. The command is offered only when
there is such output, which takes a shell that marks it with OSC 133: the palette
does not list it in a shell that sends no marks, after a command that printed nothing,
while an application holds the alternate screen, or once the row where the output
began has left scrollback. Fish 4 and Nushell send the marks unaided. Bash needs them
added to its prompt, for example in `~/.bashrc`:

```sh
PROMPT_COMMAND='printf "\033]133;D;%s\a" "$?"'
PS0='\e]133;C\a'
PS1='\[\e]133;A\a\]'$PS1
```

Output that does not end in a newline shares its last row with the next prompt, and
that row is left out.

The same evaluation fills the project panel, above the
palette hint: the project's `name`, its `runtime` type and runnable task count, the variables
in `env`, and each named set of overrides in `env.environment`. It says what the active tab's
exact directory declares, not what its shell has loaded, and it resolves nothing. A secret
shows as `•••• onepassword`, and parsing keeps only that resolver's name, so its reference
and an `exec` secret's command never reach the UI. A passthrough shows as `from $NAME`.
A value written out in CUE is shown as written, so a literal token is on screen like any
other value. A tooltip holds the whole value. Floating, the panel stops at 420px and scrolls; pinned,
it takes the sidebar's height. A directory without a project, tasks, or a notice shows no
pill, leaving the wordmark, and a pinned sidebar says there is no project there.
Changing directory clears it.

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
