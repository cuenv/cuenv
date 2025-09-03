{
  description = "cuenv - Configuration utilities and validation engine";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    crate2nix = {
      url = "github:nix-community/crate2nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  # Configure FlakeHub Cache (read-only) and enable flakes features for all consumers
  nixConfig = {
    extra-substituters = [
      "https://cache.nixos.org"
      "https://cache.flakehub.com"
    ];
    extra-trusted-public-keys = [
      "cache.flakehub.com-3:hJuILl5sVK4iKm86JzgdXW12Y2Hwd5G07qKtHTOcDCM="
    ];
    experimental-features = [ "nix-command" "flakes" ];
    accept-flake-config = true;
  };

  outputs = { self, nixpkgs, crate2nix, flake-utils, rust-overlay, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs { inherit system overlays; };

        # Pre-build the Go CUE bridge for reproducible builds
        cue-bridge = pkgs.stdenv.mkDerivation {
          pname = "libcue-bridge";
          version = "0.1.0";
          src = ./crates/cuengine;

          nativeBuildInputs = with pkgs; [
            go_1_24
          ];

          buildPhase = ''
            runHook preBuild
            
            # Build both debug and release versions
            mkdir -p $out/debug $out/release
            
            # Build debug version
            go build -buildmode=c-archive -o $out/debug/libcue_bridge.a bridge.go
            cp libcue_bridge.h $out/debug/
            
            # Build release version (with optimizations)
            CGO_ENABLED=1 go build -ldflags="-s -w" -buildmode=c-archive -o $out/release/libcue_bridge.a bridge.go  
            cp libcue_bridge.h $out/release/

            runHook postBuild
          '';

          installPhase = ''
            runHook preInstall
            # Already copied to $out during buildPhase
            runHook postInstall
          '';

          meta = with pkgs.lib; {
            description = "Go CUE bridge library for cuenv";
            license = with licenses; [ mit asl20 ];
            platforms = platforms.unix ++ platforms.windows;
          };
        };

        # Generate Cargo.nix from Cargo.lock using crate2nix
        cargoNix = crate2nix.tools.${system}.appliedCargoNix {
          name = "cuenv";
          src = ./.;
        };

        # Import the generated Cargo.nix with custom overrides
        crate2nixProject = import cargoNix {
          inherit pkgs;
          
          # Override build for cuengine to include Go bridge
          defaultCrateOverrides = pkgs.defaultCrateOverrides // {
            cuengine = attrs: {
              nativeBuildInputs = (attrs.nativeBuildInputs or []) ++ [
                pkgs.go_1_24
                pkgs.pkg-config
              ];
              
              buildInputs = (attrs.buildInputs or []) ++ 
                pkgs.lib.optionals pkgs.stdenv.isDarwin [
                  pkgs.darwin.apple_sdk.frameworks.Security
                  pkgs.darwin.apple_sdk.frameworks.CoreFoundation
                  pkgs.darwin.apple_sdk.frameworks.SystemConfiguration
                ];

              # Make prebuilt bridge available during build
              preBuild = ''
                ${attrs.preBuild or ""}
                
                # Copy prebuilt bridge artifacts to expected locations
                mkdir -p target/debug target/release
                cp -r ${cue-bridge}/debug/* target/debug/ || true
                cp -r ${cue-bridge}/release/* target/release/ || true
              '';
              
              # Set environment variables for the build
              CUE_BRIDGE_PATH = cue-bridge;
            };
          };
        };

      in
      {
        # Package outputs
        packages = {
          default = crate2nixProject.workspaceMembers.cuenv-cli.build;
          cuenv = crate2nixProject.workspaceMembers.cuenv-cli.build;
          cuenv-core = crate2nixProject.workspaceMembers.cuenv-core.build;
          cuengine = crate2nixProject.workspaceMembers.cuengine.build;
          cue-bridge = cue-bridge;
        };

        # Development shell
        devShells.default = pkgs.mkShell {
          buildInputs = with pkgs; [
            # Rust toolchain (stable with MSRV support)
            (rust-bin.stable.latest.default.override {
              extensions = [
                "cargo"
                "clippy" 
                "rust-analyzer"
                "rustc"
                "rustfmt"
                "llvm-tools-preview"
              ];
            })
            
            # Go toolchain - pinned to 1.24.x as per Phase 3 requirements
            go_1_24
            
            # Development tools
            cargo-edit
            cargo-machete
            cargo-outdated
            cargo-llvm-cov
            cargo-audit
            cargo-nextest
            cargo-release
            cargo-deny
            cargo-cyclonedx
            
            # Nix tools
            crate2nix.packages.${system}.crate2nix
            
            # CI/CD tools
            git
            gh
            jq
            prettier
            nixpkgs-fmt
            treefmt
            
            # Build dependencies
            pkg-config
            llvmPackages.bintools
          ] ++ lib.optionals stdenv.isDarwin [
            darwin.apple_sdk.frameworks.Security
            darwin.apple_sdk.frameworks.CoreFoundation
            darwin.apple_sdk.frameworks.SystemConfiguration
          ];

          # Make prebuilt bridge available in development
          shellHook = ''
            export CUE_BRIDGE_PATH="${cue-bridge}"
            
            # Ensure target directories exist and copy prebuilt bridge
            mkdir -p target/debug target/release
            if [ -d "${cue-bridge}/debug" ]; then
              cp -r ${cue-bridge}/debug/* target/debug/ 2>/dev/null || true
            fi
            if [ -d "${cue-bridge}/release" ]; then
              cp -r ${cue-bridge}/release/* target/release/ 2>/dev/null || true
            fi
            
            echo "🦀 cuenv development environment ready!"
            echo "📦 Prebuilt CUE bridge available at: ${cue-bridge}"
            echo "🔧 Use 'crate2nix generate' to update Cargo.nix when dependencies change"
            echo "🚀 Phase 3 tooling: cargo deny, audit, cyclonedx available"
          '';
        };

        # Apps
        apps = {
          default = flake-utils.lib.mkApp {
            drv = self.packages.${system}.cuenv;
            name = "cuenv";
          };
          
          # Generate Cargo.nix helper
          generate-cargo-nix = flake-utils.lib.mkApp {
            drv = pkgs.writeShellScriptBin "generate-cargo-nix" ''
              ${crate2nix.packages.${system}.crate2nix}/bin/crate2nix generate
            '';
          };
        };
      }
    );
}
