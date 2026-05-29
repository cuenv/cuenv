---
title: Install cuenv
description: Install cuenv from a release binary, Homebrew, Nix, or source, then verify it works.
---

cuenv is a single static binary. One typed `env.cue` replaces your `.env` file, your `Makefile`, and a pile of CI YAML — but first you need the `cuenv` command on your `PATH`.

Pick the method that fits your machine. The release-binary and Homebrew paths are the fastest; Nix and source builds are there when you want them.

:::note
Releases publish binaries for `darwin-arm64` (Apple Silicon), `linux-x64`, and
`linux-arm64`. Native Windows is not a release target — use
[WSL2](https://learn.microsoft.com/windows/wsl/install) and follow the Linux
steps.
:::

## Release binary (fastest)

Every release attaches prebuilt binaries to the [GitHub releases page](https://github.com/cuenv/cuenv/releases). Download the asset for your platform, make it executable, and move it onto your `PATH`.

```bash
# Pick the asset for your platform:
#   cuenv-darwin-arm64   (macOS, Apple Silicon)
#   cuenv-linux-x64      (Linux, x86_64)
#   cuenv-linux-arm64    (Linux, aarch64)
ASSET=cuenv-linux-x64

curl -fsSL -o cuenv \
  "https://github.com/cuenv/cuenv/releases/latest/download/${ASSET}"

chmod +x cuenv
sudo mv cuenv /usr/local/bin/cuenv
```

No `sudo`? Drop the binary somewhere already on your `PATH`, such as `~/.local/bin`:

```bash
mkdir -p ~/.local/bin
mv cuenv ~/.local/bin/cuenv
# Make sure ~/.local/bin is on your PATH (add to your shell rc file if not):
export PATH="$HOME/.local/bin:$PATH"
```

## Homebrew

cuenv ships a Homebrew tap at [`cuenv/homebrew-tap`](https://github.com/cuenv/homebrew-tap) that is updated automatically on every release:

```bash
brew install cuenv/tap/cuenv
```

This works on macOS (Apple Silicon) and Linux (x86_64 and arm64). The tap is also the supported way to install cuenv in CI without Nix — see the runnable [`examples/ci-cuenv-homebrew`](https://github.com/cuenv/cuenv/tree/main/examples/ci-cuenv-homebrew) project.

## Nix

If you use Nix with flakes enabled, install directly from the repository flake:

```bash
# Install into your Nix profile
nix profile install github:cuenv/cuenv

# Or run it once without installing
nix run github:cuenv/cuenv -- --help
```

For project development environments, cuenv loads your `flake.nix` dev shell automatically through its Nix runtime and `#NixFlake` hook. See [Nix Integration](/how-to/nix/) for the full workflow.

## From source

Building from source gives you the latest `main`. You need a recent Rust toolchain and a C toolchain plus OpenSSL headers (the FFI bridge to CUE links against Go).

:::caution
cuenv requires **Rust 1.85.0 or later** (Edition 2024). Older toolchains will
not compile the workspace. Update with `rustup update` if needed.
:::

```bash
git clone https://github.com/cuenv/cuenv.git
cd cuenv

# Build and install the CLI binary (crate: crates/cuenv, bin: cuenv)
cargo install --path crates/cuenv
```

Build dependencies: a C compiler and OpenSSL development headers. On Debian/Ubuntu that is `build-essential pkg-config libssl-dev`; on Fedora/RHEL `gcc pkg-config openssl-devel`; on macOS the Xcode Command Line Tools (`xcode-select --install`).

:::note
cuenv is not currently published to crates.io, so `cargo install cuenv` will not
work. Use a release binary, Homebrew, Nix, or `cargo install --path
crates/cuenv` from a clone.
:::

## Verify the install

Confirm the binary is on your `PATH` and can evaluate a minimal config:

```bash
cuenv version

mkdir cuenv-smoke-test && cd cuenv-smoke-test
cat > env.cue <<'CUE'
package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
	name: "smoke-test"
	env: {
		GREETING: "hello from cuenv"
	}
}
CUE

cuenv env print
```

You should see `GREETING` in the printed environment. If `cuenv: command not found`, the install directory is not on your `PATH` — see [Troubleshooting](/how-to/troubleshooting/).

## Shell integration

cuenv can load and unload a project's environment automatically as you `cd` between directories. Add the appropriate line to your shell config:

```bash
# ~/.bashrc
source <(cuenv shell init bash)
```

```zsh
# ~/.zshrc
source <(cuenv shell init zsh)
```

```fish
# ~/.config/fish/config.fish
cuenv shell init fish | source
```

Supported shells are `bash`, `zsh`, and `fish`. For the full command surface, tab-completion setup, and `cuenv env export`, see the [CLI reference](/reference/cli/).

## Editor setup

For CUE syntax highlighting and language-server support in your editor, see [Editor Setup](/how-to/editor-setup/).

## Next steps

- Build your first project in the [Quick Start tutorial](/tutorials/first-project/).
- Define [typed environments](/how-to/typed-environments/) instead of `.env` files.
- Resolve [secrets at runtime](/how-to/secrets/) from 1Password, AWS, and more.
- Replace your `Makefile` with [task orchestration](/how-to/run-tasks/).
- Check the [schema status](/reference/schema/status/) page before relying on any feature.
