# cuetty — Graphite Redesign

**Date:** 2026-05-12
**Scope:** `apps/cuetty/` UI shell — replace horizontal top tab bar with a vertical sidebar; refresh palette, typography, and chrome to a calmer industrial aesthetic ("Graphite"). No changes to terminal substrate, PTY plumbing, or split-tree logic.

## Goals

1. Tabs run vertically along a left sidebar instead of horizontally across the top.
2. Reduce chrome to two regions: titlebar + shell (sidebar + workspace). No bottom status bar.
3. Move new-tab / split actions into the titlebar's top-right.
4. Establish a small, consistent color and type token set so future theme work touches one module.
5. Look "slick and clean" — restrained palette, single amber accent used only where it earns attention.

## Non-goals

- Theme switching at runtime.
- Tab reordering, renaming, or persistence across launches.
- Bundling Archivo or JetBrains Mono with the binary (rely on system fallback).
- Moving status information that's being removed anywhere else — it's gone.
- Changes to PTY setup, Ghostty integration, resize math, output pump, or split-tree algorithms.

## Visual reference

The approved mockup lives at `/tmp/cuetty-mockups/02-graphite-console.html`. It is not committed to the repo; treat the spec below as authoritative.

## Layout

Window grid is two rows: titlebar then shell.

| Region    | Height / Width        | Purpose                                        |
| --------- | --------------------- | ---------------------------------------------- |
| Titlebar  | 40px                  | wordmark, location, action triplet             |
| Sidebar   | 220px (fixed)         | vertical tab list                              |
| Workspace | flex                  | active tab's split tree of terminal panes      |

Shell row: `grid-template-columns: 220px 1fr`. Workspace fills the remainder of the window height (`window_height - 40`).

Split tree behavior is unchanged. Pane borders:
- Inactive pane: `1px --rule`
- Active pane: `1px --accent`
- Divider between splits: `1px --rule`

## Titlebar (left → right)

1. **OS traffic lights.** Rendered by macOS via unified titlebar (transparent appearance). No custom lights drawn by cuetty.
2. **Wordmark.** `cuetty` in Archivo 600 / 13px / `--ink`. Letter-spacing `-0.005em`.
3. **Location string.** JetBrains Mono 11.5px. Format: `<tab_name> · <pane_cwd>` where the tab name is `--ink-2` and the path is `--sub`. The `·` separator is `--sub-2`. Example: `02 build · ~/cuenv/crates/cuenv-core`.
4. **Spacer.** Pushes actions to the right.
5. **Action triplet.** Three 28×28 buttons, 4px rounded, 2px gap.
   - `+` — New tab (`⌘T`). Hover: bg `rgba(255,255,255,0.04)`, color `--accent`.
   - `|` — Split right (`⌘D`). Hover: bg `rgba(255,255,255,0.04)`, color `--ink`.
   - `—` — Split down (`⌘⇧D`). Hover: bg `rgba(255,255,255,0.04)`, color `--ink`.
   - At rest all three are `--sub`, no background.
   - Glyphs rendered in JetBrains Mono 14px.

The cycle-pane action (`FocusNextPane`, `⌘]`) keeps its keybinding but has no UI surface. The close-tab action (`CloseTab`) keeps `⌘W`. All keybindings remain as defined in `apps/cuetty/src/lib.rs::bind_keys`.

## Sidebar

Background `--panel`, right border `1px --rule`. Padding: 12px top/bottom, 8px left/right.

Tab list is a vertical flex column; rows are siblings with `1px` margin between (not borders).

Each tab row:
- Grid: `22px 1fr`, column gap 12px.
- Padding: 12px on all sides.
- Border-radius: 4px.
- Hover background: `rgba(255,255,255,0.025)`, transition `background 120ms ease`.

Tab row content:
- **Number column** (22px, right-aligned): JetBrains Mono 11px `--sub`. Two-digit zero-padded (`01`, `02`, ...).
- **Label column** (flex):
  - **Name** (line 1): Archivo 500 13px `--ink-2`. Letter-spacing `-0.005em`. Whitespace nowrap, overflow ellipsis. Default value `Tab N` (where N is 1-indexed); user-rename deferred.
  - **Sub** (line 2): JetBrains Mono 10.5px `--sub-2`. Content is best-effort metadata: pane count, shell, working dir basename. Format suggestion: `<pane_count> pane[s] · <shell>` for multi-pane tabs, or `<cwd_basename> · <shell>` for single-pane. Exact rules deferred to implementation; render an empty string rather than placeholder text when data is missing.

Active tab:
- Background `rgba(255,255,255,0.04)`.
- 2px `--accent` left edge: implementation should reserve a 2px left strip on every row (transparent for inactive, `--accent` for active) so the active state does not cause horizontal layout shift. Full row height is acceptable; the 8px vertical inset shown in the mockup is a refinement, not a requirement.
- Number switches to `--accent`.
- Name switches to `--ink`.

Close affordance:
- Rendered only on row hover.
- Glyph `×` in JetBrains Mono 12px, 16×16 grid-center, top-right of the row.
- Default color `--sub-2`; hover color a soft red (`#d35a4e`).
- Clicking calls the same `close_tab(index)` path the keyboard `⌘W` uses today.

## Color tokens

These are the only colors the cuetty UI should reference. Every existing `rgb(0x…)` literal in `apps/cuetty/src/ui.rs` is replaced with a token from this set.

| Token    | Hex      | Used for                                          |
| -------- | -------- | ------------------------------------------------- |
| `bg`     | `#131517` | Window background, workspace background           |
| `panel`  | `#181a1d` | Sidebar background                                |
| `rule`   | `#24272c` | Borders (titlebar bottom, sidebar right edge, inactive pane border, split dividers) |
| `ink`    | `#e9e3d4` | Highest-priority text (wordmark, active tab name) |
| `ink_2`  | `#b9b3a5` | Default UI text (inactive tab names, location tab-name segment) |
| `sub`    | `#7d7669` | Secondary text (location path, tab numbers at rest, action glyphs at rest) |
| `sub_2`  | `#524d44` | Tertiary text (tab sub line, separators in location) |
| `accent` | `#f5a623` | Active tab number, active tab edge bar, active pane border, `+`-hover glyph color |

The amber accent appears, by design, in only four UI loci: active tab number, active tab edge bar, active pane border, and `+`-hover. The close-`×` hover color (`#d35a4e`) is treated as a one-off and is not promoted to a token. Terminal-side ANSI colors are out of scope of this design and continue to follow Ghostty defaults.

## Typography

Two families:
- **Archivo** (Google Fonts) for UI labels — weights 400, 500, 600.
- **JetBrains Mono** for the terminal and all monospaced UI numerics.

Type scale:

| Use                       | Family             | Size    | Weight | Color   |
| ------------------------- | ------------------ | ------- | ------ | ------- |
| Wordmark                  | Archivo            | 13px    | 600    | `ink`   |
| Title location            | JetBrains Mono     | 11.5px  | 400    | `sub` / `ink_2` for tab name part |
| Title action glyph        | JetBrains Mono     | 14px    | 400    | `sub` (resting) |
| Tab number                | JetBrains Mono     | 11px    | 400    | `sub` / `accent` active |
| Tab name                  | Archivo            | 13px    | 500    | `ink_2` / `ink` active |
| Tab sub                   | JetBrains Mono     | 10.5px  | 400    | `sub_2` |

Fonts are resolved through the system font stack with the named families as preferred. No font files are bundled in this iteration. If the system lacks Archivo, GPUI's fallback (SF Pro on macOS) is acceptable; documented as a follow-up to bundle.

## Motion

- Hover background transitions: 120ms ease on tab rows and titlebar action buttons.
- Cursor blink: handled by the Ghostty terminal view; cuetty does not animate.
- No other animations.

## State and data flow

Only what changes from today's `RootView`.

The `RootView` already owns `tabs`, `panes`, `active_tab`. Sidebar consumes that state read-only. Titlebar consumes a derived view: the active tab's title and the active pane's working directory (best effort — fall back to empty string if not available; tracking working directory is deferred and the path may show the spawn dir).

Action wiring:
- Sidebar tab row click → existing `activate_tab(index, ..)`.
- Sidebar tab close × click → existing `close_tab(index, ..)`.
- Titlebar `+` click → existing `open_tab(window, cx)`.
- Titlebar `|` click → existing `split_active_pane(SplitAxis::Row, ..)`.
- Titlebar `—` click → existing `split_active_pane(SplitAxis::Column, ..)`.
- Keyboard actions (`NewTab`, `CloseTab`, `SplitRight`, `SplitDown`, `FocusNextPane`) unchanged.

## Code-level changes in `apps/cuetty/src/ui.rs`

### Module structure

Add a new private module `theme` at the top of `ui.rs` (or a sibling file `theme.rs` and `mod theme;`) containing:

```rust
pub(crate) struct Theme;

impl Theme {
    pub const BG: u32     = 0x131517;
    pub const PANEL: u32  = 0x181a1d;
    pub const RULE: u32   = 0x24272c;
    pub const INK: u32    = 0xe9e3d4;
    pub const INK_2: u32  = 0xb9b3a5;
    pub const SUB: u32    = 0x7d7669;
    pub const SUB_2: u32  = 0x524d44;
    pub const ACCENT: u32 = 0xf5a623;
}
```

All `rgb(0x…)` literals in `ui.rs` become `rgb(Theme::*)`.

### Constants

Replace:
```rust
const TAB_BAR_HEIGHT: f32 = 38.0;
const STATUS_BAR_HEIGHT: f32 = 24.0;
const DIVIDER_THICKNESS: f32 = 1.0;
```

with:
```rust
const TITLEBAR_HEIGHT: f32 = 40.0;
const SIDEBAR_WIDTH: f32 = 220.0;
const DIVIDER_THICKNESS: f32 = 1.0;
```

### Render tree

`RootView::render` becomes a 2-row column:

```
div (root, --bg, --ink text)
├── render_titlebar(cx)
└── div (shell row, flex_row, flex_1)
    ├── render_sidebar(cx)        // 220px
    └── render_workspace(cx)      // flex_1, contains split tree
```

New methods on `RootView`:
- `render_titlebar(&self, cx) -> AnyElement`
- `render_sidebar(&self, cx) -> AnyElement`
- `render_titlebar_action(&self, kind: TitleAction, cx) -> AnyElement` — kind is `New | SplitRight | SplitDown`
- `render_tab_row(&self, input: TabRowInput, cx) -> AnyElement`

Removed:
- `render_tab_bar`, `render_tab_button`, `render_tab_close_button` (replaced by sidebar variants)
- `render_toolbar_button`
- `render_status_bar`
- `ToolbarAction`, `ToolbarButtonInput`

### `terminal_region`

```rust
fn terminal_region(window: &mut Window) -> PixelRegion {
    let size = window.viewport_size();
    PixelRegion {
        width: (f32::from(size.width) - SIDEBAR_WIDTH).max(1.0),
        height: (f32::from(size.height) - TITLEBAR_HEIGHT).max(1.0),
    }
}
```

### Active pane border

`render_pane`: switch the active border color from `rgb(0x4e8cff)` to `rgb(Theme::ACCENT)`. The inactive color becomes `rgb(Theme::RULE)`.

## Window setup (`apps/cuetty/src/main.rs`)

`apps/cuetty/src/lib.rs::window_options` already constructs `TitlebarOptions` with a `title`. To draw cuetty content inside the macOS titlebar area (so the wordmark, location, and actions appear at the same vertical position as the traffic lights), the existing options must be extended:

- Inspect the `TitlebarOptions` fields exposed by the pinned `zed-industries/zed` gpui revision (already a workspace dependency).
- Expected fields include `appears_transparent` and `traffic_light_position`. Set `appears_transparent: true` so the OS titlebar becomes transparent; cuetty's titlebar element then renders behind/through it.
- The cuetty titlebar element must reserve enough left-side space to clear the traffic lights on macOS (~80px). Use a left padding or spacer in `render_titlebar` to avoid overlapping the lights.

Fallback if `appears_transparent` is unavailable in the pinned gpui revision: render the 40px chrome strip immediately below the OS titlebar. This produces a slightly less elegant double-strip but functionally meets every other requirement in this spec. Choosing the fallback should be flagged in the PR.

## Testing

Existing tests in `ui.rs` cover split-tree behavior; they continue to pass unchanged. New verification:

- `cargo test --locked --all-targets -p cuetty` passes.
- `cargo clippy --locked --all-targets -- -D warnings` passes (no new warnings).
- `cargo fmt --all -- --check` passes.
- Manual smoke test under `nix run .#cuetty --accept-flake-config`:
  1. App opens with one tab visible in the sidebar, no status bar, titlebar action triplet visible at top-right.
  2. `⌘T` adds a tab to the sidebar.
  3. Clicking a sidebar tab activates it; active row shows amber number + amber edge bar.
  4. `⌘D` and `⌘⇧D` create splits; active pane shows a 1px `--accent` border.
  5. `⌘]` cycles pane focus (no UI button, keyboard only).
  6. Hover on a tab row reveals the `×`; clicking closes the tab (with at least one tab always retained).
  7. Resizing the window resizes the workspace correctly; sidebar width stays fixed at 220px.

## Open implementation questions (resolve during plan)

1. Exact GPUI API for unified/transparent macOS titlebar in the pinned `zed-industries/zed` revision.
2. Whether `gpui_ghostty_terminal`'s `default_terminal_font` should be overridden to JetBrains Mono explicitly, or left to the user's system default. Spec recommendation: leave default for this iteration; track as follow-up.
3. Source of the path string in the titlebar — current `TerminalProcessOptions` carries the spawn directory; track-current-cwd is out of scope.

## Out-of-scope follow-ups

- Bundle Archivo and JetBrains Mono with the app for guaranteed rendering.
- Runtime theme system supporting alternate palettes (Cobalt, Editorial, Phosphor).
- Track per-pane current working directory and surface it in the titlebar location string.
- Persistent tab/pane layout across cuetty restarts.
- Tab rename and reorder.
