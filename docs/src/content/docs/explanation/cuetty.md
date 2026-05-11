---
title: Cuetty
description: Cuetty terminal app architecture and development workflow
---

Cuetty is the experimental cuenv terminal app in `apps/cuetty`. It is scaffolded as a standalone Rust desktop application with a local Nix flake so the app can move quickly without changing the root cuenv release pipeline.

## Architecture

Cuetty uses GPUI for the desktop shell and the Ghostty VT stack for terminal parsing and rendering. Unlike Termy, it does not depend on `alacritty_terminal`.

The first milestone is deliberately small:

- Open one GPUI window.
- Spawn the user's login shell through a PTY.
- Set `TERM=xterm-256color`, `COLORTERM=truecolor`, and `TERM_PROGRAM=cuetty`.
- Stream PTY output into a Ghostty-backed terminal view.
- Forward terminal input back to the PTY.
- Reply to terminal capability queries, including Device Attributes, without waiting for a render pass.
- Install a Cuetty app menu and `cmd-q` quit action instead of inheriting dependency metadata.
- Resize the terminal state and PTY from GPUI window bounds.

The ignored `apps/cuetty/termy` checkout is a reference only. Cuetty should keep its own module boundaries and use Termy as a guide, not as a long-term dependency.

## Development

Use the app-local flake from `apps/cuetty`:

```bash
nix develop --accept-flake-config
nix run .#cuetty --accept-flake-config
nix build .#cuetty -L --accept-flake-config
nix flake check -L --accept-flake-config
```

The app flake owns Cuetty's GPUI, Ghostty, Zig, and Rust tool acquisition. Root cuenv checks still need to pass before committing changes to the repository.
