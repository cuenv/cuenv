# Homebrew Tap for cuenv

This repository contains the Homebrew formula for installing cuenv.

## Installation

```bash
brew install cuenv/cuenv/cuenv
```

Or tap the repository first:

```bash
brew tap cuenv/cuenv
brew install cuenv
```

### Install from HEAD (latest development version)

```bash
brew install cuenv --HEAD
```

## Updating

To update cuenv to the latest version:

```bash
brew upgrade cuenv
```

## Uninstall

```bash
brew uninstall cuenv
brew untap cuenv/cuenv
```

## About

cuenv is a modern application build toolchain with typed environments and CUE-powered task orchestration.

For more information, visit:

- **Homepage**: https://github.com/cuenv/cuenv
- **Documentation**: https://docs.cuenv.sh (coming soon)

## Building from Source

If you prefer to build from source:

```bash
git clone https://github.com/cuenv/cuenv
cd cuenv
cargo build --release --package cuenv-cli
```

The binary will be available at `target/release/cuenv`.

## Notes

- This formula requires Rust and Go to be installed as build dependencies
- The formula builds the `cuenv-cli` package from the workspace
- For development, you may want to install from HEAD to get the latest features

## License

cuenv is licensed under AGPL-3.0-or-later.
