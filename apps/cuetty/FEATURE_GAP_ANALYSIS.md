# Cuetty feature-gap analysis

This plan compares the current Cuetty GPUI/Rio proof of concept with the two
primary references from the local study: [Termy](https://github.com/lassejlv/termy)
and [Okena](https://github.com/contember/okena). It treats their README feature
lists as claims to validate, not as acceptance evidence. Nebula and
`gpui-terminal` remain secondary renderer references.

## Baseline

Cuetty currently has:

- Rio-owned PTY and VT state behind an object-safe `TerminalSession` trait.
- Semantic `TerminalFrame` colors and style flags.
- A replaceable `TerminalRenderer` implemented as one fixed-layout GPUI element.
- Batched text shaping with ligatures disabled, explicit font fallbacks, merged
  backgrounds, cursor layering, and a shared 16px/1.2 line-height contract.
- Resize, keyboard input, paste, OSC 52 clipboard writes, close handling, and
  focused live shell behavior.
- A resizable vertical tab rail (50px compact to 280px expanded), session-only
  Settings with Apply/Cancel/Reset, and a native Quit/Cmd-Q action.
- Static validation plus a signed macOS bundle check, local process launch, and
  a direct interactive shell/history smoke pass. The real daily-workload corpus
  still needs human acceptance.

The known live-UI limits are incomplete IME/emoji presentation and
visible-frame-only selection/search coordinates. Rio-owned scrollback is live
through wheel/trackpad and keyboard navigation. Mouse selection/Cmd-C copy and
literal visible-frame search are live,
including grid-painted selection and current-match overlays;
the GPUI shell creates independent local Rio sessions for tabs. Split actions
remain rejected; the product never renders a fake pane. Pure persistence,
session, and capability contracts remain
behind replaceable traits.

## Gap matrix

| Capability | Cuetty | Termy reference | Okena reference | Priority |
| --- | --- | --- | --- | --- |
| PTY + ANSI/VT engine | Rio adapter; working | Alacritty-based runtime | Alacritty-based runtime | Keep Rio |
| Fixed-grid rendering | Custom GPUI element; live verified | Custom element, batches, damage cache | Custom element, batches, cache | Done; add regressions |
| Cell width/line metrics | Explicit font/line contract; snapped once | Configurable multiplier and measured advance | Configurable multiplier and measured advance | P0 |
| Unicode width/combining | Backend cluster text and wide occupancy preserved; IME/emoji rendering incomplete | Rich render-cell text/width model | Rich render-cell text/width model | P0 |
| Cursor and terminal modes | Cursor shape/visibility and application cursor keys observed; mouse/keypad/focus modes incomplete | Cursor styles, keyboard/mouse modes | Cursor/mouse modes and richer metadata | P0 |
| Damage and render cache | Coalesced wakeups; full-frame paint | Dirty spans and shaped-line cache | Damage-aware batched rendering | P0 |
| Clipboard and paste | Cmd-V, OSC 52, mouse selection and Cmd-C copy | Broader clipboard/selection workflows | Clipboard, image paste, selection | P1 |
| Scrollback and selection | Rio-owned history navigation live; selection remains visible-viewport-only | Present | Present | P1 |
| Search | Literal visible-frame search live; regex/full history staged | Present | Inline/regex search | P1 |
| Fonts, themes, configuration | Initial explicit stack/theme | Configurable themes, keybindings, fonts | Settings, themes, zoom, shell choice | P1 |
| Tabs, splits, focus navigation | Independent local Rio tabs; resizable 50px compact rail; split actions explicitly unavailable | Tabs, splits, layouts | Tabs, splits, detachable panes | P2 |
| Workspace persistence | Versioned codec and atomic file store; no live restore | Reusable layouts | Workspace/session restore | P2 |
| Session persistence | Child shell only | Optional tmux control mode | dtach/tmux/screen backends | P3 |
| Remote/API/plugin surface | Missing | FFI/native SDK and plugin/runtime crates | Remote HTTP/WebSocket and hooks | P3 |
| Graphics/image protocols | Missing | Validate separately | Image paste; protocol scope to verify | P3 |
| Cross-platform release | Architecture is portable; only macOS verified | macOS/Linux/Windows target | Desktop plus mobile/remote work | P3 |

## Execution sequence

### P0 — terminal correctness

1. Replace `TerminalCell::codepoint` with a renderer-neutral text/span model:
   grapheme or cell text, width, continuation/placeholder state, style, and
   optional hyperlink metadata.
2. Add Rio adapter tests for combining marks, wide CJK, emoji, soft wraps,
   cursor positions, and explicit background/foreground semantics. Keep Rio
   types behind the adapter.
3. Add mouse reporting, bracketed paste, application cursor/key modes, and
   cursor shape/visibility where the pinned Rio API permits it. Keep unsupported
   Rio capabilities explicit rather than inventing state.
4. Introduce damage spans and shaped-line/background caches behind traits so the
   renderer can repaint only changed rows without coupling cache policy to Rio.
5. Add a golden terminal fixture suite: shell startup, ANSI colors, resize,
   Unicode, cursor movement, OSC 52, and shutdown.

**Exit gate:** the same PTY/frame contract passes the fixture suite on macOS and
Linux, and a live resize plus Unicode probe has visual evidence.

### P1 — usable daily terminal

1. Extend selection and search across Rio's authoritative history without
   duplicating terminal state.
2. Wire copy, paste, selection export, and literal search into the live surface
   (regex remains a later capability).
3. Move font, theme, line height, cursor, and keybindings into a serializable
   configuration trait with sane platform defaults.
4. Add the Termy/Okena box-drawing, block, sextant, and braille geometry path
   where font glyphs cannot provide stable full-cell coverage.

**Exit gate:** a user can select/copy/search a long colored Unicode transcript,
change font/line-height, restart, and retain the intended settings.

### P2 — product shell

1. Model tabs/splits as a pure layout tree with replaceable pane content.
2. Add focus traversal, pane resizing, close/reopen, and detachable-window
   boundaries only after the layout tree is tested without GPUI.
3. Persist layouts and terminal metadata through a versioned store; do not put
   persistence in `TerminalSession`.
4. Add a command palette/settings surface after actions have stable trait-backed
   identifiers.

**Current state:** every live tab owns an independent local Rio session. Session
creation is transactional and callbacks are generation-scoped; close first
removes the exact session then commits the preflighted tab removal. Split actions
remain rejected with a visible notice. Pane resizing, reopen, detachable
windows, live restore, and multi-session split allocation remain staged.

**Exit gate:** split/tab/layout operations are deterministic in unit tests and
restore correctly after a clean restart.

### P3 — integrations and reach

1. Add a session-backend trait and optional tmux/dtach adapters; the default
   local Rio shell must remain dependency-free.
2. Define a capability-scoped plugin/remote trait before exposing HTTP,
   WebSocket, or Wasm surfaces. Keep permissions and serialization host-owned.
3. Qualify image/Kitty graphics and IME behavior as separate protocol work.
4. Add Linux and Windows CI/build lanes only after P0 fixtures are portable.
5. Generate a dependency, asset, and license report for every distributable
   build.

## Architecture guardrails

- `TerminalSession`: PTY/engine lifecycle, input, resize, frame reads, and
  control effects. Rio remains one implementation, not a leaked type.
- `TerminalFrame`/render model: semantic, backend-neutral, width-aware data.
- `TerminalRenderer`: GPUI element and paint/cache policy; no PTY calls.
- `TerminalLayout`: tabs/splits/focus tree; no GPUI or Rio types.
- `TerminalConfig`: serializable fonts, metrics, themes, keybindings, and
  feature flags.
- `WorkspaceStore` and `SessionBackend`: persistence/reconnection seams,
  introduced only when their contracts have tests.

Do not fork Termy wholesale or replace Rio with Alacritty merely to obtain its
features. Reuse MIT-licensed design/code only with its copyright and license
notice; audit every dependency, font, icon, and asset before distribution.

## Evidence to collect per tranche

- feature matrix updated with observed source locations and verified runtime
  behavior;
- unit/integration tests for the trait boundary;
- one live screenshot or interaction transcript for visual behavior;
- explicit unsupported-capability notes;
- license/dependency delta;
- no claim of completion until the relevant exit gate passes.
