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

### Method 1: From Crates.io (Recommended)

:::note
This method will be available once cuenv reaches stable release.
:::

```bash
cargo install cuenv-cli
```

### Method 2: From GitHub Releases

Download pre-built binaries from the [releases page](https://github.com/cuenv/cuenv/releases):

**Linux (x86_64)**

```bash
curl -LO https://github.com/cuenv/cuenv/releases/latest/download/cuenv-linux-x86_64.tar.gz
tar xzf cuenv-linux-x86_64.tar.gz
sudo mv cuenv /usr/local/bin/
```

**macOS (Intel)**

```bash
curl -LO https://github.com/cuenv/cuenv/releases/latest/download/cuenv-macos-x86_64.tar.gz
tar xzf cuenv-macos-x86_64.tar.gz
sudo mv cuenv /usr/local/bin/
```

**macOS (Apple Silicon)**

```bash
curl -LO https://github.com/cuenv/cuenv/releases/latest/download/cuenv-macos-aarch64.tar.gz
tar xzf cuenv-macos-aarch64.tar.gz
sudo mv cuenv /usr/local/bin/
```

### Method 3: From Source

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

### Method 4: Using Nix

If you have Nix installed:

```bash
# Install from flake
nix profile install github:cuenv/cuenv

# Or run directly
nix run github:cuenv/cuenv -- --help
```

### Method 5: Development Environment

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
cargo install cuenv-cli
```

#### Fedora/RHEL

```bash
# Install dependencies
sudo dnf install gcc pkg-config openssl-devel

# Install Rust if not already installed
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env

# Install cuenv
cargo install cuenv-cli
```

#### Arch Linux

```bash
# Install dependencies
sudo pacman -S base-devel openssl pkg-config

# Install Rust if not already installed
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env

# Install cuenv
cargo install cuenv-cli
```

### macOS

#### Using Homebrew

:::note
Homebrew formula coming soon.
:::

```bash
# Future homebrew installation
brew install cuenv
```

#### Manual Installation

```bash
# Install Xcode Command Line Tools
xcode-select --install

# Install Rust if not already installed
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source ~/.cargo/env

# Install cuenv
cargo install cuenv-cli
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
cargo install cuenv-cli
```

## Verification

After installation, verify cuenv is working correctly:

```bash
# Check version
cuenv --version

# Verify installation
cuenv doctor

# Test basic functionality
mkdir test-cuenv
cd test-cuenv
cuenv init
cuenv validate
```

## Shell Integration

### Bash

Add to `~/.bashrc`:

```bash
# cuenv shell integration
eval "$(cuenv init --shell bash)"
```

### Zsh

Add to `~/.zshrc`:

```zsh
# cuenv shell integration
eval "$(cuenv init --shell zsh)"
```

### Fish

Add to `~/.config/fish/config.fish`:

```fish
# cuenv shell integration
cuenv init --shell fish | source
```

### Nushell

Add to `~/.config/nushell/config.nu`:

```nushell
# cuenv shell integration
cuenv init --shell nu
```

## IDE Integration

### Visual Studio Code

Install the cuenv extension:

```bash
# Install from marketplace
code --install-extension cuenv.cuenv-vscode
```

### IntelliJ/CLion

Install the cuenv plugin from JetBrains Marketplace.

### Vim/Neovim

Add cuenv support with vim-cue:

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

1. Check the [troubleshooting guide](/troubleshooting/)
2. Search existing [GitHub issues](https://github.com/cuenv/cuenv/issues)
3. Create a new issue with:
   - Your operating system and version
   - Rust version (`rustc --version`)
   - Installation method used
   - Complete error message

## Next Steps

After installation:

- Follow the [Quick Start guide](/quick-start/)
- Explore [configuration options](/configuration/)
- Learn about [task orchestration](/tasks/)
- Set up your first [typed environment](/environments/)
