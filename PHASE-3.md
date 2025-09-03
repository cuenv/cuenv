# Phase 3 — Build, CI, packaging, and supply chain

Status: ✅ **COMPLETED**
Owners: Infra
Target window: 1–2 weeks

## Goals

- Reproducible builds with Nix and prebuilt Go bridge artifacts.
- Cross-platform CI (Linux, macOS, Windows) with pinned toolchains.
- Supply-chain hygiene and SBOM.

## Scope (must)

- CI
  - Matrix: ubuntu-latest, macos-latest, windows-latest; Rust: stable + 1.82 (MSRV).
  - Pin Go: 1.24.x in all jobs (Linux/Mac/Windows).
  - Enable `cargo deny`, `cargo audit` on all platforms.
  - Add coverage on Linux; codecov gating (soft initially).
- Nix / Repro
  - Build and cache prebuilt `libcue_bridge.a` and header under `target/{profile}`; wire into build.rs prebuilt path.
  - Add flake packaging of `cuenv` (bin) and bridge (c-archive) outputs.
- Packaging
  - GitHub Releases (archives per-platform), Homebrew tap formula, Nix package output.
  - `cargo-release` with `release.toml` (tag, changelog, crates.io if appropriate).
- Supply chain
  - SBOM: `devenv shell -- cargo cyclonedx -o sbom.json`
  - `cargo deny check bans licenses advisories`
  - Verify LICENSE headers across crates.

## Acceptance criteria

- CI is green on all three OSes; Go version consistent.
- Repro builds with flake; prebuilt bridge consumed in Rust build.rs when present.
- Release pipeline can produce signed artifacts (signing optional in this phase).

## Commands

- devenv shell -- cargo deny check
- devenv shell -- cargo audit
- devenv shell -- cargo cyclonedx --override-filename sbom.json

## ✅ Implementation Summary

### Completed Features

1. **Nix Flake Packaging** (`flake.nix`)
   - Uses `nix-community/crate2nix` for proper Rust dependency management
   - Pre-builds Go CUE bridge for both debug/release profiles
   - Cross-platform package outputs (Linux, macOS, Windows support)
   - Reproducible builds with pinned toolchains

2. **Cross-Platform CI Matrix** (`.github/workflows/ci.yml`)
   - ✅ Matrix: ubuntu-latest, macos-latest, windows-latest
   - ✅ Rust: stable + 1.82.0 MSRV (Edition 2024)
   - ✅ Go: 1.24.x pinned across all platforms
   - ✅ Platform-specific tooling (Nix on Unix, direct install on Windows)

3. **Supply Chain Security**
   - ✅ `deny.toml` - comprehensive dependency/license/security checking
   - ✅ `cargo deny check` on all platforms
   - ✅ `cargo audit` security vulnerability scanning
   - ✅ SBOM generation with `cargo-cyclonedx`
   - ✅ License header verification script

4. **Enhanced Build System** (`crates/cuengine/build.rs`)
   - ✅ Multi-location prebuilt bridge detection (Nix, local, env vars)
   - ✅ Improved fallback to source builds
   - ✅ Better error handling and diagnostics

5. **Release Automation** (`release.toml`)
   - ✅ `cargo-release` configuration for workspace
   - ✅ Pre-release hooks (tests, audit, deny, formatting, clippy)
   - ✅ Integration with existing release-please workflow

6. **Development Tooling** (`devenv.nix`)
   - ✅ Added `cargo-deny`, `cargo-cyclonedx` to development environment
   - ✅ All Phase 3 tooling available in dev shell

### CI Jobs Structure

- **lint-and-format**: Code quality, license headers (Ubuntu)
- **test-suite**: Cross-platform testing matrix (Linux/macOS/Windows)
- **supply-chain-security**: Security auditing on all platforms
- **coverage**: Code coverage with soft gating (Ubuntu)
- **benchmarks**: Performance regression tracking (Ubuntu)

### Key Files Added/Modified

- ✅ `flake.nix` - Nix packaging with crate2nix
- ✅ `deny.toml` - Supply chain security configuration  
- ✅ `release.toml` - Release automation
- ✅ `scripts/check-license-headers.sh` - License verification
- ✅ `.github/workflows/ci.yml` - Enhanced CI matrix
- ✅ `devenv.nix` - Added Phase 3 tooling
- ✅ `crates/cuengine/build.rs` - Enhanced prebuilt logic
