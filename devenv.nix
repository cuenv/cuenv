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
  
  # Add Go language support (required for cuengine FFI)
  languages.go = {
    enable = true;
    package = pkgs.go_1_24;
  };

  packages = with pkgs; [
    # Existing tools
    cargo-edit
    cargo-machete
    cargo-outdated
    cargo-llvm-cov
    llvmPackages.bintools
    
    # CI/CD tools
    cargo-audit        # Security vulnerability scanning
    cargo-nextest      # Faster test runner for CI
    cargo-release      # Release automation helper
    git                # Required for release-please
    gh                 # GitHub CLI for release automation
    jq                 # JSON processing for scripts
  ];
  
  # Code formatting with treefmt
  treefmt = {
    enable = true;
    projectRoot = ./.;
    
    programs = {
      rustfmt.enable = true;
      gofmt.enable = true;
      nixpkgs-fmt.enable = true;
      prettier = {
        enable = true;
        includes = [
          "*.json"
          "*.yml"
          "*.yaml"
          "*.md"
        ];
        excludes = [
          "Cargo.lock"
          "*.toml"
        ];
      };
    };
  };
}
