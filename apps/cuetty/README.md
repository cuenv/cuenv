# Cuetty

Cuetty is a small Cuenv terminal app scaffolded from Termy's GPUI shape, but with a Ghostty-backed terminal core instead of `alacritty_terminal`.

## Current Scope

- GPUI desktop window.
- PTY-backed login shells.
- Ghostty VT parsing and rendering through the pinned `gpui_ghostty_terminal` stack.
- Tabs with one or more terminal panes per tab.
- Right and down splits backed by independent terminal sessions.
- Active-pane focus cycling and per-pane resize calculations.
- Immediate terminal capability query responses for shells such as fish.
- Native Cuetty app menu, window title, and keyboard shortcuts.
- App-local Nix flake for reproducible tools, checks, and builds.

The nested `termy/` checkout is intentionally ignored and kept only as a reference while Cuetty develops its own app shape.

## Development

```bash
nix develop --accept-flake-config
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --all -- --check
```

Run the app:

```bash
nix run .#cuetty --accept-flake-config
```

Build and check with Nix:

```bash
nix build .#cuetty -L --accept-flake-config
nix flake check -L --accept-flake-config
```

## Architecture

`src/pty.rs` owns process setup, PTY I/O threads, shell environment, and resize-safe grid dimensions. `src/terminal_responses.rs` handles small PTY query responses that need to be sent before rendering, such as Device Attributes. `src/ui.rs` owns the GPUI root view, tab model, split tree, Ghostty terminal sessions, input bridge, output pumps, active-pane tracking, and resize observer.

The current UI shell deliberately keeps tabs and splits app-owned while terminal parsing, rendering, keyboard input, selection, and clipboard operations still flow through `gpui_ghostty_terminal`. Native embedded Ghostty remains the next architecture decision, but the app now has the session boundary needed to swap the terminal substrate without redesigning the product shell.
