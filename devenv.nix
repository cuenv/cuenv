{ pkgs, ... }:
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

  packages = with pkgs; [
    # Adds the set-version command
    cargo-edit
    # Find unused crates
    cargo-machete
    # Find outdated crates
    cargo-outdated
    # Code coverage tool
    cargo-llvm-cov
    # LLVM tools for coverage
    llvmPackages.bintools
  ];
}
