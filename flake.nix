{
  description = "cuenv - Configuration utilities and validation engine";

  inputs = {
    flake-schemas.url = "https://flakehub.com/f/DeterminateSystems/flake-schemas/*";

    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

    crate2nix = {
      url = "github:nix-community/crate2nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };

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

  outputs = { self, flake-schemas, nixpkgs, crate2nix, rust-overlay, ... }:
    let
      supportedSystems = [ "x86_64-linux" "aarch64-linux" "x86_64-darwin" "aarch64-darwin" ];
      forEachSupportedSystem = f: nixpkgs.lib.genAttrs supportedSystems (system: f {
        inherit system;
        pkgs = import nixpkgs {
          inherit system;
          overlays = [
            rust-overlay.overlays.default
            (final: prev: {
              rustToolchain = final.rust-bin.stable.latest.default.override {
                extensions = [ "rust-src" "rust-analyzer" "clippy" "rustfmt" "llvm-tools-preview" ];
              };
              # Override defaultCrateOverrides to be an empty set to avoid darwin.apple_sdk_11_0
              defaultCrateOverrides = { };
            })
          ];
        };
      });
      # Helper to build the CUE bridge without tying devShells to self.packages
      mkCueBridge = pkgs: pkgs.buildGoModule {
        pname = "libcue-bridge";
        version = "0.1.0";
        src = ./crates/cuengine;

        # The vendorHash is required for reproducible Go builds.
        vendorHash = "sha256-mU40RCeO0R286fxfgONJ7kw6kFDHPMUzHw8sjsBgiRg";

        buildInputs = pkgs.lib.optionals pkgs.stdenv.isDarwin (
          if pkgs ? darwin.apple_sdk then [
            pkgs.darwin.apple_sdk.frameworks.CoreFoundation
            pkgs.darwin.apple_sdk.frameworks.Security
            pkgs.darwin.apple_sdk.frameworks.SystemConfiguration
          ] else [
            pkgs.darwin.CoreFoundation
            pkgs.darwin.Security
            pkgs.darwin.SystemConfiguration
          ]
        );

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
    in
    {
      schemas = flake-schemas.schemas;

      packages = forEachSupportedSystem ({ system, pkgs }:
        let
          # Pre-build the Go CUE bridge for reproducible builds
          cue-bridge = mkCueBridge pkgs;

          # Import Cargo.nix - pkgs.defaultCrateOverrides is already overridden to be empty
          crate2nixProject = import ./Cargo.nix {
            inherit pkgs;
            # Provide our crate-specific overrides
            defaultCrateOverrides = {
              cuengine = attrs: {
                nativeBuildInputs = (attrs.nativeBuildInputs or [ ]) ++ [
                  pkgs.go_1_24
                  pkgs.pkg-config
                ];

                buildInputs = (attrs.buildInputs or [ ]) ++
                  pkgs.lib.optionals pkgs.stdenv.isDarwin (
                    if pkgs ? darwin.apple_sdk then [
                      pkgs.darwin.apple_sdk.frameworks.CoreFoundation
                      pkgs.darwin.apple_sdk.frameworks.Security
                      pkgs.darwin.apple_sdk.frameworks.SystemConfiguration
                    ] else [
                      pkgs.darwin.CoreFoundation
                      pkgs.darwin.Security
                      pkgs.darwin.SystemConfiguration
                    ]
                  );

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
          default = crate2nixProject.workspaceMembers.cuenv-cli.build;
          cuenv = crate2nixProject.workspaceMembers.cuenv-cli.build;
          cuenv-core = crate2nixProject.workspaceMembers.cuenv-core.build;
          cuengine = crate2nixProject.workspaceMembers.cuengine.build;
          cue-bridge = cue-bridge;
        });

      devShells = forEachSupportedSystem ({ system, pkgs }:
        {
          default = let cue-bridge = mkCueBridge pkgs; in pkgs.mkShell {
            packages = with pkgs; [
              # Rust toolchain
              rustToolchain

              # Go toolchain - pinned to 1.24.x as per Phase 3 requirements
              go_1_24

              # CUE language support
              cue

              # Documentation tools
              antora

              # Development tools
              # Note: Some cargo tools may trigger darwin.apple_sdk_11_0 issues
              # Excluded: cargo-edit, cargo-machete, cargo-outdated, cargo-llvm-cov
              # cargo-release, cargo-cyclonedx (they cause SDK issues on Darwin)
              cargo-audit
              cargo-nextest
              cargo-deny

              # Nix tools (use directly from input without referencing the package)
              # crate2nix is available via the 'generate-cargo-nix' app (see 'apps' below)
              # Run with: nix run .#generate-cargo-nix

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
            ] ++ pkgs.lib.optionals pkgs.stdenv.isDarwin (
              if pkgs ? darwin.apple_sdk then [
                pkgs.darwin.apple_sdk.frameworks.CoreFoundation
                pkgs.darwin.apple_sdk.frameworks.Security
                pkgs.darwin.apple_sdk.frameworks.SystemConfiguration
              ] else [
                pkgs.darwin.CoreFoundation
                pkgs.darwin.Security
                pkgs.darwin.SystemConfiguration
              ]
            );

            env = {
              RUST_BACKTRACE = "1";
              RUST_SRC_PATH = "${pkgs.rustToolchain}/lib/rustlib/src/rust/library";
            };

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
        });

      apps = forEachSupportedSystem ({ system, pkgs }: {
        default = {
          type = "app";
          program = "${self.packages.${system}.cuenv}/bin/cuenv";
        };

        generate-cargo-nix = {
          type = "app";
          program = "${pkgs.writeShellScriptBin "generate-cargo-nix" ''
              ${crate2nix.packages.${system}.crate2nix}/bin/crate2nix generate
            ''}/bin/generate-cargo-nix";
        };
      });
    };
}
