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
  leaking the launcher's nesting into the prompt. Each shell receives
  `TERM_PROGRAM=cuetty` and `TERM_PROGRAM_VERSION` for its Cuetty version as
  part of its child environment.
- If a pane's measurements become unavailable, Cuetty blocks input to that
  pane and offers to close its tab. If the window's shared measurements fail,
  it offers to close the window instead.
- Fixed-width, fixed-row-height GPUI rendering with semantic colours and
  explicit wide-cell and soft-wrap occupancy.
- Box drawing, block elements, shades, and Powerline separators drawn as shapes on
  the cell's device pixels, so rules and blocks meet with no seam at any line height.
- A cursor that fades when the application asks it to blink, turns to a hollow outline
  without keyboard focus, covers both columns of a wide character, and asks for no
  frames at rest, so an idle window stops drawing.
- Text shaped with the font's contextual alternates and placed cell by cell: every
  glyph starts on the column its source text belongs to, so substitutions, combining
  marks, and fallback fonts cannot shift a column.
- Four complete themes, or `auto` to follow the desktop's light or dark, shared by Rio's
  semantic resolver and the GPUI chrome. Catppuccin Mocha is the default.
- Ghostty is the visual reference for restrained native chrome: traffic lights,
  one title strip that shares the terminal's surface, a sidebar floating inset from
  the window's edges so it sits comfortably with rounded corners, stable single-line
  tab labels, and a full-bleed terminal.
- Keyboard input, including macOS Control chords, paste, resize, title updates, clipboard copy, mouse
  selection, Rio-owned history navigation, and visible-frame literal search. A mouse selection is
  copied to the system clipboard on release; an empty click leaves it alone. Holding a selection
  drag on the top or bottom row scrolls through history until the pointer moves away or is released.
  Selection and its copied text use Rio's history; changing column width invalidates the selection.
- A thin overlay scrollbar in the terminal's right gutter, where there is history to scroll.
  The command palette covers it while open.
  It shows while the view moves, holds a moment, and fades over 150ms, and comes back when
  the pointer reaches the padding it sits in. Its thumb can be dragged and a press beside it jumps there.
  It takes no room from the grid, and an application's wheel or an alternate screen with no
  history leaves it hidden. With less right padding than its 12px strip the thumb lies over
  the last column, which stays the terminal's: crossing it reveals nothing, a selection
  dragged through it carries on, and only a thumb the view's moving has just shown takes
  a press.
- A search bar at the terminal's top right while a search is open, from Cmd-F (Ctrl-Shift-F
  on Linux): the query, where navigation has got to or how many matches there are, and
  buttons for the previous and next match and for closing it, which are Shift-Enter, Enter,
  and Escape from the keyboard. The search covers what is on screen, as its placeholder says.
  It narrows with the terminal so the query stays in view.
- Every match of a search on screen is tinted, with the current one in its own colours on
  top. The search follows output as it moves the text, keeping to the match it was on or
  the nearest after it, and stays open as the view scrolls, searching what comes into view.
- Wheel and trackpad input normally navigates Rio history. An application that
  enables a supported mouse-reporting mode receives wheel reports instead; an
  alternate-screen application with alternate scroll enabled and mouse
  reporting disabled receives arrow keys. Shift-wheel requests local history
  when available in the current screen. An isolated Linux Xvfb/Niri run with
  Codex 0.166.0 confirmed wheel-up and wheel-down through a local transcript.
  Physical device input and macOS still need interactive verification.
- The active pane's post-layout GPUI canvas is the source of truth for each Rio
  resize, so the measured width already excludes the title strip, terminal
  inset, and the sidebar in its current state instead of leaving a stale
  frame clipped in the viewport, so toggling the sidebar or the title strip resizes the
  grid within a few frames, even at an idle prompt. The sidebar's panel slides over
  150ms while the room it is given moves in one step, at the start of opening and at
  the end of closing, so a toggle resizes the grid and tells the PTY once rather than
  on every frame of the animation.
- The terminal surface defaults to a 14px inset from the host chrome; the
  inset uses the same terminal surface colour and is subtracted from the Rio grid
  dimensions as well as the rendered bounds. Its opposing edges constrain the
  surface size so the last row remains inside the content panel. Space left over
  past the last whole column and row shares that one background layer, so no
  strip appears at the grid's right or bottom edge. A TUI that paints its own
  explicit background still stops at the grid.
- Independent live Rio sessions for each tab with a replaceable workspace
  model. Cmd-T creates a session-backed tab and Cmd-W closes the active one;
  a new tab starts in the current tab's local foreground process directory when
  available, otherwise it uses a reported directory that still exists or `$HOME`,
  split shortcuts are rejected with a visible notice until pane/session
  allocation is implemented, and the UI never presents a fake terminal pane.
  Application shortcuts written as Cmd-* use Command on macOS and Ctrl-Shift
  on Linux, so plain Ctrl chords such as Ctrl-C reach the shell; the one exception
  is Ctrl-1 to Ctrl-9 selecting a tab that exists.
  Ctrl-Page Up and Ctrl-Page Down switch to the previous and next tab,
  wrapping at either end. The command palette is configurable (see below).
- A right-side tab sidebar, 200px wide and listing tabs by name, that collapses
  to 56px with its toggle or Cmd-B (Ctrl-Shift-B on Linux). Collapsed, each tab
  shows an initial taken from its user-set name, project banner, or directory, so it
  stays put as programs come and go, with the full name as a tooltip. A tab is named after, in order, the name
  the user gave it, the Cuenv task it is running, the program in its foreground or
  a title that program set, and finally its directory; with none of those it is
  `Terminal`. A task launched from Cuetty names its tab until `cuenv` hands the
  foreground back or another program takes it. A title lasts only as long as the
  program it was set under, a title that merely names a shell is ignored, a title that
  names a directory stands in only while the working directory is unknown, a foreground
  that cannot be named keeps what was last known (a pipeline whose first command has
  exited, as in `git log | less`, cannot be), a shell running a script is named after
  the script, and Nix's
  `.name-wrapped` binaries are shown as `name`. A tab list taller than the window scrolls, and
  follows the active tab.
- A close button revealed on an expanded tab's row when the pointer is over it. It takes
  over from the tab's indicator while it shows. A collapsed row has none, because it is too
  narrow for one to stay clear of where a click selects the tab: select it and use the
  close chord, Cmd-W (Ctrl-Shift-W on Linux).
- Double-clicking a tab, or Cmd-R (Ctrl-Shift-R on Linux), edits its name in place,
  expanding the sidebar if it is collapsed. Enter or clicking elsewhere keeps the name and
  Escape abandons the edit. The name then outranks every name Cuetty derives until it is
  emptied, which hands the tab back to its derived name.
- Dragging a tab onto another moves it to that tab's place in the list, which the number
  keys and tab switching then follow.
- The terminal fades back slightly while its window does not have the keyboard, and the
  cursor becomes an outline, so it is clear at a glance where typing would go. Its
  colours move towards its background, so drawn glyphs stay even and a translucent
  window's material does not change; emoji, which bring their own colours, stay as they
  are. Chrome keeps its colours, and the fade costs an idle window no redraws.
- Chrome that moves briefly, and only in answer to something: the sidebar's controls and
  rows and their close buttons fade their hover over 100ms, and on a switch of tab one
  marker slides to its new place over 140ms while the row fills hand over. The command palette fades in over 120ms,
  already taking keys as it does. A hover stays true when the chrome moves under a still
  pointer, as when the tab list scrolls. Nothing draws once it has landed, and
  `motion: "reduced"`, or the platform's own setting, makes all of it land at once.
- A designed state in place of a terminal that cannot be shown. A tab whose session broke
  says what went wrong and offers Restart shell, which starts a fresh shell in the same tab
  and the same directory with the same look, or Close tab. A directory that has since gone
  starts it in the home directory instead. A window with no terminal, because no shell would
  start, says why and offers New tab. Enter takes the first of these and the usual chords
  keep working. Restarting that fails stays put and says why.
- Three elevations for whatever floats over the terminal, each one token for its shadow,
  outline, fill, and corners: tooltips and a dragged tab sit just off the surface, the
  search bar floats, and the command palette is the highest thing in the window.
- A hint at the foot of the expanded sidebar naming the active tab's command palette chord,
  such as `⌘K` or `Ctrl+Shift+K`. Clicking it opens the palette.
- A muted second line on each expanded tab, such as `cuenv · main`: its directory,
  unless the tab is already named after it, and the git branch that directory is on, or
  a detached commit's short form. Cuetty reads the repository's `HEAD` file every couple
  of seconds and spawns no process, following `.git` files for worktrees and submodules.
  With neither known the line is simply absent.
- A project panel in the expanded sidebar, for an active tab whose exact directory holds a
  `package cuenv`: the project's name, its runtime type and runnable task count, the variables
  in `env`, and each named set of overrides in `env.environment`. It says what the CUE declares,
  not what the shell has loaded. Nothing is resolved: a secret appears as `•••• onepassword`, and
  its reference is dropped while parsing, while a value written out in CUE is shown as written.
  A passthrough appears as `from $NAME`, and a tooltip holds a whole value. The panel scrolls
  past 240px, gives way to the tab list in a short window, and has no place in the collapsed sidebar.
- A small indicator on each tab, beside its name or below its collapsed mark, with its
  meaning in the tab's tooltip: a bell for a bell rung in a tab not on screen, which
  includes every tab while the window is in the background, a ring
  while a task launched from Cuetty runs, counted from the Enter that runs it if it
  needed arguments, a flag once that task hands the foreground
  back, which says it ended and not how, and a dot for text that changed in a tab not on screen. Every state has its own
  shape, so colour is never all that tells them apart. Arriving at a tab, or back at the
  window with it on screen, clears what was waiting there, and typing moves on from a finished task. Only tasks launched from
  Cuetty count as running, since a program in the foreground may just as well be an
  editor left open.
- A shell that exits with a failing status or dies on a signal leaves its tab open
  with its final output on screen, a warning triangle on the tab, and a strip below the
  output saying how it ended. The strip takes its room from the grid rather than covering
  it: blank rows go first, then the oldest rows move into scrollback, so the last lines
  stay visible, as they do if the window is resized afterwards. Search, copy, and
  scrollback still work; a fresh press of Enter or Ctrl-D, the close chord, or closing the
  tab dismiss it, while a key still held from before the shell died does not.
  The tab offers no tasks any more, since nothing is left to run them, which also hands
  a palette chord back to whatever else it means. A shell that exits cleanly closes its
  tab at once, as does
  one whose status the platform did not report. Shells exit with their last command's
  status, so `exit` straight after a failed command also leaves the tab open. Tasks
  launched from Cuetty report only that they finished: they run in the shell, and Rio
  discards the exit status shells report through OSC 133.
- One design token set for all chrome spacing, radii, type sizes, border widths,
  elevation, and colours, with faint dividers between regions, and one bundled
  Lucide icon set for chrome controls. Chrome text uses the platform's sans-serif
  UI font, or on Linux the first of Inter, Adwaita Sans, Cantarell, Ubuntu, Noto
  Sans, DejaVu Sans, and Liberation Sans that is installed; only paths and
  shortcuts use the terminal's monospaced family.
- An original app icon, a lowercase c waiting at a block cursor, which the Nix package
  installs to the hicolor theme for its desktop entry. The macOS development bundle has
  no icon yet.
- Global and per-directory CUE configuration for theme, accent, font family, font size,
  line height, padding, and transparency. There is no in-app Settings page. The native
  Cuetty menu exposes Quit Cuetty and Cmd-Q uses the same application-level
  action.
- Versioned, metadata-only workspace persistence and capability/session seams
  for later product integrations.

Termy and Okena remain reference projects for renderer and product-shell
patterns. They are not vendored dependencies and their runtime models are not
silently substituted for Rio.

## Appearance

Cuetty aims to be calm: the terminal is what you came for, and the chrome around it
should not compete with it.

- **One material.** The terminal, the title strip, and the sidebar read as one surface.
  At `transparency: 0` the whole window is opaque; above that they share a single opacity
  over the native blur, rather than a pane of glass beside a pane of paint.
- **Neutral chrome, one accent.** The chrome takes its surfaces and text from the
  [theme](#themes). A single accent colours only small marks: the active tab's marker,
  the notice dot, an unread tab, the caret. A project's `rail` recolours that marker and
  nothing else, so no setting can fill a region with colour.
- **The sidebar is on the right.** It is 200px wide and lists tabs by name, and collapses
  to a 56px strip of initials; `sidebar` chooses how a window starts.
- **Type does the hierarchy.** Chrome text is the platform's sans-serif on a small scale
  of 11 to 14px with three text colours; only paths and shortcuts are monospaced.
- **Depth is for what floats.** The command palette, tooltips, and search bar
  are opaque, with one shadow per level of elevation, softer on a light theme.
- **One token set.** All of this comes from `apps/cuetty/src/terminal/chrome/tokens.rs`
  and the theme, so chrome views write no literal colours, sizes, or radii.
- **Identity stays small.** A muted `cuetty` wordmark in the title strip, and an
  original icon for the launcher.

Everything here that is a choice is a setting: `theme`, `accent`, `rail`, `transparency`,
`titlebar`, `sidebar`, `font`, `padding`, `cursor`, `motion`, and `minimumContrast`, all
described under [Cuetty CUE configuration](#cuetty-cue-configuration).

## Cuetty CUE configuration

For the active Rio tab, Cuetty asks Rio for the shell's current working
directory and evaluates package `cuetty` in that exact directory. The nearest
`cue.mod/module.cue` establishes the module root only; configuration is not
inherited from parent directories and any `.cue` filename may provide the
package. A directory without package `cuetty` is neutral. Standalone packages
are evaluated in the exact directory; imports that require a module context
fail through normal CUE evaluation. A matching package may provide a
single-line `banner` and a quoted `rail: "#RRGGBB"`. The banner joins the tab's
name in its tooltip; `rail` is the project accent, so `rail: "#ff0000"` colours the
small marker on the active tab while that tab is active and never fills the
sidebar. Terminal panes are never framed, and the former `border` field is
rejected as unknown. The title strip shows the active tab's name on the terminal's
own surface, with a small muted `cuetty` wordmark at its far end, and the window takes the same name as its title in every titlebar state,
for window switchers and compositors. `titlebar: false` hides it while that tab is active and lets the
terminal extend to the top of the window; it defaults to `true`. `keybindings: palette:` rebinds the command palette; see
[Keybindings](#keybindings). Off macOS the strip draws only under client-side
decorations: Cuetty asks for server-side ones, and a window that gets them leaves its
title and moving it to the windowing system, whether or not that shows a title bar, so
`titlebar` has no effect there and the sidebar names the tab. A hidden strip leaves the small gap above the
sidebar to move the window by. GPUI cannot hide the traffic lights once a window exists, so
on macOS the terminal starts below them instead, in a band the strip's height that also
moves the window; a larger `padding.top` still applies.

Terminal appearance can be configured in the same package:

```cue
package cuetty

theme: "graphite"
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

| Field | Meaning and allowed values | Default |
| --- | --- | --- |
| `font.family` | Nonempty font family name | `MonaspiceNe Nerd Font` |
| `font.size` | Font size in pixels, 1–128 | 16 |
| `font.lineHeight` | Line-height multiplier, 0.5–3 | 1.2 |
| `font.calt` | Boolean: the font's contextual alternates, such as Monaspace's texture healing. Standard and contextual ligatures (`liga`, `clig`) stay off either way, and turning this off helps with a font whose `calt` draws ligatures | `true` |
| `font.boldWeight` | Integer CSS weight for bold text, 100–900. A family without that exact weight uses its nearest | 600 |
| `padding.top`, `padding.right`, `padding.bottom`, `padding.left` | Per-edge integer pixels, 0–128 | 14 each |
| `cursor.blink` | `"terminal"` blinks as the running application asks, leaving the cursor steady at most prompts and when the backend cannot report a blink request; `"on"` and `"off"` override it. A blink fades rather than toggles, holds solid for a moment after output, a key press, or a switch of tab, and settles solid after about half a minute of nothing happening | `"terminal"` |
| `cursor.trail` | Boolean: a short fading trail behind a cursor that jumps, along a row or from one row to another. Moving a single cell, as typing does, leaves none | `false` |
| `motion` | `"reduced"` makes the chrome's transitions land at once and turns the cursor's trail off; `"full"` always animates; `"system"` follows GNOME's `enable-animations` or macOS's Reduce Motion, read once as a window opens, and animates where neither can be read | `"system"` |
| `minimumContrast` | Contrast ratio from 1 to 21 that text is kept to against what it is drawn on, including a selection; 1 is off. Colours an application spelled out in 24-bit; graphics characters, meaning box and block elements, Powerline and Powerline Extra separators, Braille, and the legacy computing symbols; and the cursor and search match, which have their own colours, are left alone so image previews and Powerline segments still line up | 1 |
| `transparency` | Integer percent, 0–100; the terminal canvas, title strip, and sidebar share this opacity | 0 |
| `theme` | `"auto"`, `"catppuccin-mocha"`, `"catppuccin-latte"`, `"graphite"`, or `"paper"`: the whole palette for the terminal and the chrome. See [Themes](#themes) | `"catppuccin-mocha"` |
| `accent` | Quoted `#RRGGBB`; the window's accent, for instance to match a compositor's window border. See [Accent colours](#accent-colours) | The theme's accent |
| `sidebar` | `"expanded"` or `"collapsed"`: how a window's sidebar starts. A window opens with the global package's look and sidebar when that evaluates within half a second, so its first frame is already the right size, then its first directory's evaluation settles the setting. Only that first configuration is read, so changing directory or editing the value later does not move it, and the toggle and its shortcut outrank it for the life of the window | `"expanded"` |

Fields are optional. The active tab's settings determine the rendered grid,
terminal inset, and window material. At `transparency: 0` the whole window is
opaque. Above 0, the terminal and structural chrome share one opacity over the
native blur, so the window reads as one material. The sidebar has no fill of its own
there, since a second layer would be more opaque than the rest of the window, so its
outline draws the panel and hover and active-tab fills sit directly on the material.
Those marks stop fading at 50% opacity, so a very transparent window still shows the
panel and gives feedback; text, the active tab's marker, and the floating
task panels and their outlines stay opaque for legibility. Unknown top-level and nested
fields are rejected.

The cuenv repository includes a root `cuetty.cue` with a `cuenv` banner and a
Catppuccin Mocha blue project accent. It applies while the shell is in the repository
root; subdirectories can provide their own `package cuetty` configuration.
When using the CUE CLI at the root, select the package explicitly, for example
`cue export .:cuetty` or `cue export .:cuenv`.

### Themes

`theme` picks one of the complete palettes Cuetty ships. Each one sets the terminal's
default colours and ANSI palette along with every chrome colour, so there is nothing to
assemble and no colour to get wrong.

| Theme | Character |
| --- | --- |
| `catppuccin-mocha` | The default: soft dark blue-greys with a blue accent |
| `catppuccin-latte` | Catppuccin's light flavour. Metadata tones, the cursor, and all six ANSI hues, red and blue included, are deepened from the published palette to hold the contrast below |
| `graphite` | Neutral dark greys with a quiet steel accent |
| `paper` | Warm light: ink on uncoated paper with a rust accent |

`theme: "auto"` uses Latte while the desktop is light and Mocha while it is dark, and
switches a running window as soon as the desktop does, with no reload and without
touching the grid; an `accent` holds across the switch. macOS reports its appearance
directly. On Linux it comes from the XDG desktop portal's colour-scheme setting, which
arrives just after a window opens, so a dark desktop can show one light frame first. A
desktop with no portal, or one that states no preference, counts as light there, so
choose a theme by name on such a system.

Applications that ask the terminal about colours (OSC 4, 10, 11, and 12), as Neovim and
`bat` do to tell a light background from a dark one, are told the palette their tab
paints in, so they pick the right scheme on a light theme. A shell starts already knowing
that palette, so its startup files can ask. They are told once, at the time they ask: a
program that has already chosen stays with its choice when `auto` switches underneath it,
although everything it left in the default colours follows. Colours an application sets
for itself are neither painted nor reported.

Three moments can still give an earlier answer, because the real one is not known yet. A
window's first shell waits half a second at most for the global configuration, and after
that starts with the default palette. On Linux `auto` sees the appearance the window opens
with until the portal answers. And a shell that has just changed directory answers for the
look it carries over until that directory's configuration has evaluated, which does not
wait for its tasks to be discovered.

A unit test holds every theme to the same contrast: body and metadata text at least
4.5:1 on each surface it is painted on, ANSI colours 4.5:1 on the page, on a selection,
and on a search match apart from the one faint neutral pair (black on a dark theme, white
on a light one), which only has to stay visible on them, and the accent
and the cursor at least 3:1. Floating panels cast half the shadow on a light theme,
since black at the same strength reads as a smudge on a pale page.

### Accent colours

Two settings colour the chrome, and neither ever fills a region:

- `accent` is the window's accent. It marks the active tab, the integration notice,
  and the command palette's caret. Set it in the global package to match something
  outside Cuetty, such as a compositor's window border.
- `rail` is a project's accent. It colours only the active tab's marker, and only
  while that tab is in a directory that sets it.

The active tab's marker therefore uses `rail`, then `accent`, then the theme's
accent. Everything else uses `accent`, then the theme's accent. A global `accent`
and a directory's `rail` are different fields, so they never conflict during
unification. To keep the marker on your `accent` everywhere, leave `rail` unset in
the directories you control.

A project picks its `rail` without knowing which theme it will land on, so these marks
are held to 3:1 against the sidebar: a colour that already shows stays exactly as given,
and one that would vanish, such as a pale blue on `paper`, moves just far enough to read.

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
theme:    *"auto" | string
titlebar: *false | bool
font: {
    family:     *"Monaspace Neon" | string
    size:       *16 | number
    lineHeight: *1.2 | number
}
padding: {
    top:    *14 | int
    right:  *14 | int
    bottom: *14 | int
    left:   *14 | int
}
transparency: *0 | int
```

A directory can override individual fields, including one padding edge,
without redefining the other global defaults.
A directory without package `cuetty` uses the global package alone. A
conflict or an invalid global package is reported like any invalid matching
configuration and keeps the last good presentation for that directory, if one
exists. The global directory is watched alongside the active directory when it
exists; if created later while the working directory is known, it is picked up
on the next directory change. If the shell's working directory is unavailable,
Cuetty applies only the global package and checks for its directory on each poll.

#### Home Manager

Home Manager can link a CUE file you maintain as the global package:

```nix
{
  xdg.configFile."cuetty/config.cue".source = ./cuetty.cue;
}
```

The file should declare `package cuetty`. Use CUE defaults as shown above when
individual fields should be overridable per directory. If you used the former
`homeManagerModules.cuetty` module, remove its import and move your
`programs.cuetty.settings` values into this CUE file.

Directory and module-metadata changes are watched natively. Evaluation and
validation occur off the GPUI thread. Until a new directory's evaluation lands, the tab
keeps its look but drops the previous `banner` and `rail`, and a new tab starts from
the look of the tab it opened from, so neither flashes the defaults or resizes the
grid. Invalid matching CUE keeps the last
valid presentation for that directory and exposes an integration notice: a small
accent marker beside the sidebar's controls, with the message in its tooltip. The
tooltip stops at twelve lines; `cue eval .:cuetty` in that directory prints a
whole evaluation error.

The command palette lists Cuetty actions, including tabs, the sidebar, and
terminal search. Type `$` to search task, group, and sequence names from the
active tab's exact-CWD `package cuenv`; removing `$` returns to commands. The
task button opens this same task mode. The palette does not participate in
terminal layout and takes pointer input for itself; clicking outside closes it.
Results scroll under a fixed field and hint, following the keyboard selection,
and every match remains reachable. Launching a
task delegates to `cuenv task` in the same pane that supplied the catalogue, pasting
the command and pressing Enter separately, because a shell in bracketed paste mode
takes a pasted newline as text. Enter is pressed only while the foreground is an
interactive shell, judged from the name and start-up arguments of the process that
holds it: a program, or a shell running a script (`bash deploy.sh`, `sh -c …`), gets the
command but no Enter, so it cannot submit something the user was still writing or
answer a script's question. The sidebar's notice marker then says the command is
waiting, on that pane, until Enter or Ctrl-C finishes the line or the foreground
changes; a configuration error for that pane takes priority over it. A shell's builtin, such as `read`, or a subshell looks the
same as its prompt from outside, so a command launched during one does get an Enter. Required task parameters leave an editable command at the prompt. The app does not duplicate
the task graph, cache, hooks, or secret resolution.

### Keybindings

Ctrl-Page Up selects the previous tab and Ctrl-Page Down selects the next;
both wrap around the tab list. Cmd-1 to Cmd-8 on macOS, and Ctrl-1 to Ctrl-8
elsewhere, select a tab by position, and 9 always selects the last one. Holding the
modifier for a moment shows each tab's number as a faint hint. Off macOS these are
taken from applications whenever that tab exists, including the legacy aliases Ctrl-2
to Ctrl-8 for NUL, ESC, FS, GS, RS, US, and DEL. With no such tab, the key reaches the
terminal.

The command palette opens with Cmd-K on macOS and Ctrl-Shift-K elsewhere,
leaving plain Ctrl-K to the shell. The sidebar hint shows the active chord.
Rebind it from any `package cuetty`, global
or per-directory:

```cue
package cuetty

keybindings: palette: *"ctrl-shift-p" | string
```

A chord is `-`-separated modifiers (`ctrl`, `alt`, `shift`, `cmd`/`super`)
followed by a key, matched case-insensitively against that exact modifier set.
Cmd-B on macOS and Ctrl-Shift-B elsewhere collapse and expand the sidebar.
Cmd-R on macOS and Ctrl-Shift-R elsewhere rename the active tab.

A palette chord takes precedence over the tab number and Ctrl-Page chords it
conflicts with, as the binding the user chose. The palette is available even
without a task catalogue. An unparseable chord or any other
`keybindings` field is an invalid configuration.

## Current limits

The following are deliberately documented as staged work rather than implied
support:

- Full-history and regular-expression search; search covers the visible viewport
  and is literal-only.
- Application mouse reporting covers wheel events only. Clicks, drags, and
  pointer motion are not reported to applications; local text selection remains
  available. Original X10 mouse mode has no wheel reports, and legacy non-SGR
  reports have coordinate limits.
- Complete Unicode behaviour, including combining marks, ZWJ sequences,
  emoji presentation, and IME correctness.
- Kitty graphics and other image protocols.
- tmux, remote, attach, and reconnect backends.
- Rich task progress, cancellation controls, and source-linked diagnostics;
  the current task surface launches the canonical CLI in a terminal tab.
- Wasm plugins and a plugin/component-tree runtime.
- Split-pane session allocation, plus live workspace restore/reconnect. Dimming an inactive
  pane waits on this too, since a window only ever shows one pane.

Earlier implementation work recorded focused Rust validation and macOS bundle
verification. For the current direct-`rio-vt` path, Linux app compilation and
the real PTY lifecycle suite pass; private Xvfb/Niri smoke runs also exercised
initial keyboard focus, Codex transcript scrolling, drag selection through history
in both directions, automatic clipboard copying, and new-tab directory inheritance.
An Apple Silicon smoke
run is still required. The GPUI-representable Kitty and
`modifyOtherKeys` subset now has deterministic coverage; IME, focus reporting,
application keypad identity, key releases, alternate-key reporting, and
hyperlinks remain qualification items.
Human visual and input acceptance remains a separate caveat:
the terminal must still be exercised interactively for font metrics, line
spacing, selection, search focus, clipboard behaviour, resize, and shell
lifecycle before a release claim. The isolated Linux smoke is not physical
desktop acceptance; macOS and Windows still require validation of this revision.

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
cargo test --locked --workspace -- --include-ignored
cargo clippy --workspace --all-targets --all-features -- -D warnings
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

The Linux `checks.<system>.cuetty` flake output builds that same package and
checks its executable, desktop launcher, icon, and dynamically loaded library
paths. The `cuetty-test`, `cuetty-clippy`, and `cuetty-fmt` outputs run the
workspace's focused tests and checks. The test check includes the 16 PTY tests
that Cargo ordinarily ignores; they can also be run separately with `cargo test
--locked --bin cuetty -- --ignored` in the Cuetty shell.

GPUI 0.2 requires stacksafe 0.1. The Cuetty workspace patches that dependency
with a small adapter to stacksafe 1.0, so GPUI's recursive layout and drop
paths keep stack protection without depending on the unmaintained
`proc-macro-error2` crate. The adapter covers GPUI's default-feature use; it
does not implement the removed `derive-visitor` feature. Its deep-recursion
and recursive-drop tests run with the Cuetty workspace tests.
