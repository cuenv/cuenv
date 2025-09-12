{ pkgs, lib, ... }:
{
  languages.rust = {
    enable = true;
    channel = "stable";
    components = [
      "cargo"
      "clippy"
      "rust-analyzer"
      "rustc"
      "rustfmt"
      "llvm-tools-preview"
    ];
  };
  languages.cue.enable = true;
  languages.nix.enable = true;

  # Add Go language support (required for cuengine FFI)
  languages.go = {
    enable = true;
    package = pkgs.go_1_24;
  };

  packages = with pkgs; [
    # Docs
    antora

    # Existing tools
    cargo-edit
    cargo-machete
    cargo-outdated
    llvmPackages.bintools

    # CI/CD tools
    cargo-audit # Security vulnerability scanning
    cargo-nextest # Faster test runner for CI
    cargo-release # Release automation helper
    cargo-deny # Dependency and license checking
    cargo-cyclonedx # SBOM generation
    git # Required for release-please
    gh # GitHub CLI for release automation
    jq # JSON processing for scripts
    prettier # Formatter for JSON/Markdown
    nixpkgs-fmt # Formatter for Nix
    treefmt # Format everything
  ] ++ lib.optionals pkgs.stdenv.isLinux [
    # Linux-only packages (cargo-llvm-cov is broken on macOS)
    cargo-llvm-cov # Code coverage
  ];
}
