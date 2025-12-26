---
title: Installation
description: Complete installation guide for cuenv
---

This guide covers all installation methods for cuenv across different platforms and environments.

## System Requirements

### Minimum Requirements

- **Operating System**: Linux, macOS, or Windows (WSL2 recommended)
- **Rust**: 1.70 or later
- **Memory**: 4 GB RAM minimum, 8 GB recommended
- **Storage**: 1 GB free space

### Optional Dependencies

- **Nix Package Manager**: For enhanced package management features
- **Docker**: For containerized environments
- **Git**: For version control integration

## Installation Methods

### Method 1: Using Nix (Recommended)

If you have Nix installed, this is the preferred way to install cuenv:

```bash
# Install from flake
nix profile install github:cuenv/cuenv

# Or run directly
nix run github:cuenv/cuenv -- --help
```

### Method 2: Using Homebrew

For macOS and Linux users with Homebrew:

```bash
brew install cuenv/cuenv/cuenv
```

### Method 3: From Crates.io

:::note
This method will be available once cuenv reaches stable release.
:::

```bash
cargo install cuenv
```

### Method 4: From GitHub Releases

:::note
Pre-built binaries will be available once cuenv reaches stable release.
:::

Download pre-built binaries from the [releases page](https://github.com/cuenv/cuenv/releases).

### Method 5: From Source

Build from source for the latest development features:

```bash
# Clone the repository
git clone https://github.com/cuenv/cuenv.git
cd cuenv

# Build with optimizations
cargo build --release

# Install to cargo bin directory
cargo install --path crates/cuenv-cli
```

### Method 6: Development Environment

For contributors and developers:

```bash
# Clone and enter development environment
git clone https://github.com/cuenv/cuenv.git
cd cuenv

# Enter the development shell
nix develop

# Or using direnv (if configured)
direnv allow
```

## Platform-Specific Setup

### Linux

#### Ubuntu/Debian

```bash
# Install dependencies
sudo apt update
sudo apt install build-essential pkg-config libssl-dev

# Install Rust if not already installed
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env

# Install cuenv
cargo install cuenv
```

#### Fedora/RHEL

```bash
# Install dependencies
sudo dnf install gcc pkg-config openssl-devel

# Install Rust if not already installed
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env

# Install cuenv
cargo install cuenv
```

#### Arch Linux

```bash
# Install dependencies
sudo pacman -S base-devel openssl pkg-config

# Install Rust if not already installed
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env

# Install cuenv
cargo install cuenv
```

### macOS

#### Using Homebrew

```bash
brew install cuenv/cuenv/cuenv
```

#### Manual Installation

```bash
# Install Xcode Command Line Tools
xcode-select --install

# Install Rust if not already installed
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env

# Install cuenv
cargo install cuenv
```

### Windows

#### Windows Subsystem for Linux (Recommended)

```bash
# Install WSL2 and Ubuntu
wsl --install -d Ubuntu

# Follow Linux installation steps inside WSL
```

#### Native Windows

```powershell
# Install Visual Studio Build Tools
# Download from: https://visualstudio.microsoft.com/downloads/

# Install Rust
# Download from: https://rustup.rs/

# Install cuenv
cargo install cuenv
```

## Verification

After installation, verify cuenv is working correctly:

```bash
# Check version
cuenv version

# Test basic functionality
mkdir test-cuenv
cd test-cuenv
# Create a simple configuration
echo 'package cuenv' > env.cue
echo 'env: {}' >> env.cue
echo 'tasks: {}' >> env.cue

# Verify it loads
cuenv env print
```

## Shell Integration

### Bash

Add to `~/.bashrc`:

```bash
# cuenv shell integration
source <(cuenv shell init bash)

# Enable shell completions
source <(COMPLETE=bash cuenv)
```

### Zsh

Add to `~/.zshrc`:

```zsh
# cuenv shell integration
source <(cuenv shell init zsh)

# Enable shell completions
source <(COMPLETE=zsh cuenv)
```

### Fish

Add to `~/.config/fish/config.fish`:

```fish
# cuenv shell integration
cuenv shell init fish | source

# Enable shell completions
COMPLETE=fish cuenv | source
```

:::tip
Shell completions provide tab-completion for all cuenv commands, options, and task names from your CUE configuration. See the [CLI reference](/reference/cli/#shell-completions) for more details.
:::

## IDE Integration

:::note
Official IDE extensions are planned for future releases. The instructions below describe planned functionality and third-party CUE support.
:::

### Visual Studio Code

Install the official Cuenv extension for full IDE integration:

```bash
# Install the Cuenv extension
code --install-extension cuenv.cuenv-vscode

# Also install CUE language support
code --install-extension cue-lang.vscode-cue
```

See the [VSCode Extension documentation](/vscode-extension/) for features and configuration.

### IntelliJ/CLion

:::caution
The cuenv plugin is not yet available. For CUE support, use the CUE plugin from JetBrains Marketplace.
:::

### Vim/Neovim

Add CUE syntax support with vim-cue:

```vim
" Add to your vimrc
Plug 'jjo/vim-cue'
```

## Troubleshooting

### Common Issues

**Command not found**

Ensure `~/.cargo/bin` is in your PATH:

```bash
echo 'export PATH="$HOME/.cargo/bin:$PATH"' >> ~/.bashrc
source ~/.bashrc
```

**Permission denied**

On Linux/macOS, ensure the binary is executable:

```bash
chmod +x ~/.cargo/bin/cuenv
```

**Build failures**

Update Rust to the latest version:

```bash
rustup update
```

**SSL/TLS errors**

Update certificates and try again:

```bash
# Ubuntu/Debian
sudo apt update && sudo apt install ca-certificates

# macOS
brew install ca-certificates
```

### Getting Help

If you encounter issues:

1. Check the [troubleshooting guide](/how-to/troubleshooting/)
2. Search existing [GitHub issues](https://github.com/cuenv/cuenv/issues)
3. Create a new issue with:
   - Your operating system and version
   - Rust version (`rustc --version`)
   - Installation method used
   - Complete error message

## Next Steps

After installation:

- Follow the [Quick Start guide](/tutorials/first-project/)
- Explore [configuration options](/how-to/configure-a-project/)
- Learn about [task orchestration](/how-to/run-tasks/)
- Set up your first [typed environment](/how-to/typed-environments/)
