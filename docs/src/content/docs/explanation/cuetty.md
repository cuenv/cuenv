---
title: Cuetty
description: Cuetty terminal app architecture and development workflow
---

Cuetty is the experimental cuenv terminal app in `apps/cuetty`. It is scaffolded as a standalone Rust desktop application with a local Nix flake so the app can move quickly without changing the root cuenv release pipeline.

## Architecture

Cuetty uses GPUI for the desktop shell and the Ghostty VT stack for terminal parsing and rendering. Unlike Termy, it does not depend on `alacritty_terminal`.

The first milestone was deliberately small:

- Open one GPUI window.
- Spawn the user's login shell through a PTY.
- Set `TERM=xterm-256color`, `COLORTERM=truecolor`, and `TERM_PROGRAM=cuetty`.
- Stream PTY output into a Ghostty-backed terminal view.
- Forward terminal input back to the PTY.
- Reply to terminal capability queries, including Device Attributes, without waiting for a render pass.
- Install a Cuetty app menu and `cmd-q` quit action instead of inheriting dependency metadata.
- Resize the terminal state and PTY from GPUI window bounds.

The current shell milestone adds the first product boundary:

- A tab model with independent terminal sessions per tab.
- A split tree per tab for right and down splits.
- One PTY, Ghostty terminal session, input bridge, and output pump per pane.
- Active-pane tracking and focus cycling.
- Per-pane grid resizing based on the active tab's split geometry.
- App actions and keybindings for new tab, close tab, split right, split down,
  and focus next pane.

The ignored `apps/cuetty/termy` checkout is a reference only. Cuetty should keep its own module boundaries and use Termy as a guide, not as a long-term dependency.

## Integration notes

Cuetty leans on three pre-1.0 building blocks: GPUI from Zed (git pin), `gpui_ghostty_terminal` from `Xuanwo/gpui-ghostty` (git pin, vendors Ghostty's VT core via Zig), and `portable-pty` for shell I/O. This shape is currently the shortest path to a Ghostty-backed terminal on GPUI; `libghostty` itself is still working toward a stable embedding surface, and rolling our own bindings would duplicate the glue Xuanwo's crate already provides.

Two consequences worth knowing:

- **Lockstep pinning.** The `gpui_ghostty_terminal` rev in `Cargo.toml` and the `gpui-ghostty-src` flake input must always point at the same commit. The Nix build symlinks the vendored Ghostty source from that flake input into the Cargo build tree, so a mismatch yields a build that links the wrong Zig artefacts. Bump both together.
- **`terminal_responses.rs` is a temporary patch.** Upstream answers DSR and OSC color queries but not Primary or Secondary Device Attributes. Cuetty scans the PTY output stream itself to reply, so shells like fish do not stall on `CSI c` at startup. Delete the module once upstream gains DA support.

Output is event-driven: each PTY reader thread pushes byte chunks through a
`flume` channel that its GPUI task awaits asynchronously, then batches anything
else already buffered before handing the batch to that pane's terminal view.
There is no fixed-interval polling loop.

Tabs and splits are intentionally app-owned. Each leaf in the split tree points
at a terminal pane, and each pane owns its own PTY plus Ghostty-backed terminal
view. Cuetty still uses `gpui_ghostty_terminal` for the terminal substrate; the
tab and split model is the boundary that lets Cuetty later prototype a native
embedded-Ghostty adapter without rewriting the product shell.

## Development

Use the app-local flake from `apps/cuetty`:

```bash
nix develop --accept-flake-config
nix run .#cuetty --accept-flake-config
nix build .#cuetty -L --accept-flake-config
nix flake check -L --accept-flake-config
```

The app flake owns Cuetty's GPUI, Ghostty, Zig, and Rust tool acquisition. Root cuenv checks still need to pass before committing changes to the repository.
