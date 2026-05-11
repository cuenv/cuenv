# Cuetty

Cuetty is a small Cuenv terminal app scaffolded from Termy's GPUI shape, but with a Ghostty-backed terminal core instead of `alacritty_terminal`.

## Current Scope

- GPUI desktop window.
- PTY-backed login shell.
- Ghostty VT parsing and rendering through the pinned `gpui_ghostty_terminal` stack.
- Immediate terminal capability query responses for shells such as fish.
- Native Cuetty app menu, window title, and `cmd-q` quit binding on macOS.
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

`src/pty.rs` owns process setup, PTY I/O threads, shell environment, and resize-safe grid dimensions. `src/terminal_responses.rs` handles small PTY query responses that need to be sent before rendering, such as Device Attributes. `src/ui.rs` owns the GPUI root view, Ghostty terminal session, input bridge, output pump, and resize observer.
