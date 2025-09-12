{
  description = "cuenv - Configuration utilities and validation engine";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.05";

    crane = {
      url = "github:ipetkov/crane";
    };

    flake-utils.url = "github:numtide/flake-utils";

    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    advisory-db = {
      url = "github:rustsec/advisory-db";
      flake = false;
    };

    flake-schemas.url = "https://flakehub.com/f/DeterminateSystems/flake-schemas/*";
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

  outputs = { self, nixpkgs, crane, flake-utils, rust-overlay, advisory-db, flake-schemas, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };

        # Configure Rust toolchain
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" "clippy" "rustfmt" "llvm-tools-preview" ];
        };

        # Create crane lib with our custom Rust toolchain
        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

        # Helper to build the CUE bridge
        mkCueBridge = pkgs: pkgs.buildGoModule {
          pname = "libcue-bridge";
          version = "0.1.0";
          src = ./crates/cuengine;

          vendorHash = "sha256-mU40RCeO0R286fxfgONJ7kw6kFDHPMUzHw8sjsBgiRg";

          buildInputs = pkgs.lib.optionals pkgs.stdenv.isDarwin [
            pkgs.darwin.apple_sdk.frameworks.CoreFoundation
            pkgs.darwin.apple_sdk.frameworks.Security
            pkgs.darwin.apple_sdk.frameworks.SystemConfiguration
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

        # Pre-build the Go CUE bridge
        cue-bridge = mkCueBridge pkgs;

        # Filter source to include only Rust-relevant files
        src = pkgs.lib.cleanSourceWith {
          src = ./.;
          filter = path: type:
            (pkgs.lib.hasSuffix "\.rs" path) ||
            (pkgs.lib.hasSuffix "\.toml" path) ||
            (pkgs.lib.hasSuffix "\.lock" path) ||
            (pkgs.lib.hasSuffix "\.go" path) ||
            (pkgs.lib.hasSuffix "\.mod" path) ||
            (pkgs.lib.hasSuffix "\.sum" path) ||
            (pkgs.lib.hasSuffix "bridge.h" path) ||
            (type == "directory");
        };

        # Common build arguments for all derivations
        commonArgs = {
          inherit src;
          strictDeps = true;

          nativeBuildInputs = with pkgs; [
            go
            pkg-config
          ];

          buildInputs = with pkgs; [
            # Add libiconv for all platforms that need it
            libiconv
          ] ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [
            # Darwin-specific frameworks
            pkgs.darwin.apple_sdk.frameworks.CoreFoundation
            pkgs.darwin.apple_sdk.frameworks.Security
            pkgs.darwin.apple_sdk.frameworks.SystemConfiguration
          ];

          # Set up environment for the Go bridge
          preBuild = ''
            # Copy prebuilt bridge artifacts to expected locations
            mkdir -p target/debug target/release
            cp -r ${cue-bridge}/debug/* target/debug/ || true
            cp -r ${cue-bridge}/release/* target/release/ || true
          '';

          # Environment variables
          CUE_BRIDGE_PATH = cue-bridge;
        };

        # Build dependencies only (for caching)
        cargoArtifacts = craneLib.buildDepsOnly (commonArgs // {
          # Explicitly override Darwin buildInputs to ensure no legacy SDK
          buildInputs = with pkgs; [
            libiconv
          ] ++ pkgs.lib.optionals pkgs.stdenv.isDarwin [
            pkgs.darwin.apple_sdk.frameworks.CoreFoundation
            pkgs.darwin.apple_sdk.frameworks.Security
            pkgs.darwin.apple_sdk.frameworks.SystemConfiguration
          ];
        });

        # Build the workspace
        cuenv = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;
          pname = "cuenv";
          version = "0.1.0";
        });

        # Individual crate args for building specific packages
        individualCrateArgs = commonArgs // {
          inherit cargoArtifacts;
          doCheck = false; # We'll run tests separately
        };

      in
      {
        # Schema for flake
        schemas = flake-schemas.schemas;

        # Packages that can be built
        packages = {
          default = cuenv;
          cuenv = cuenv;
          cuenv-cli = craneLib.buildPackage (individualCrateArgs // {
            pname = "cuenv-cli";
            cargoExtraArgs = "-p cuenv-cli";
          });
          cue-bridge = cue-bridge;
        };

        # Checks run by `nix flake check`
        checks = {
          # Build check
          inherit cuenv;

          # Format check
          cuenv-fmt = craneLib.cargoFmt {
            inherit src;
          };

          # Clippy check
          cuenv-clippy = craneLib.cargoClippy (commonArgs // {
            inherit cargoArtifacts;
            cargoClippyExtraArgs = "--all-targets -- -D warnings";
          });

          # Doc check
          cuenv-doc = craneLib.cargoDoc (commonArgs // {
            inherit cargoArtifacts;
          });

          # Test check using nextest
          cuenv-nextest = craneLib.cargoNextest (commonArgs // {
            inherit cargoArtifacts;
            partitions = 1;
            partitionType = "count";
          });

          # Audit check
          cuenv-audit = craneLib.cargoAudit {
            inherit src advisory-db;
          };
        };

        # Development shell
        devShells.default = craneLib.devShell {
          # Inherit checks to get their inputs
          checks = self.checks.${system};

          # Additional packages for development
          packages = with pkgs; [
            # Rust toolchain (provided by crane)
            # Go toolchain
            go_1_24

            # CUE language support
            cue

            # Documentation tools
            antora

            # Development tools
            cargo-audit
            cargo-nextest
            cargo-deny
            cargo-llvm-cov

            # CI/CD tools
            git
            gh
            jq
            nodePackages.prettier
            nixpkgs-fmt
            treefmt

            # Build dependencies
            pkg-config
            llvmPackages.bintools
          ];

          # Environment variables
          RUST_BACKTRACE = "1";
          RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";
          CUE_BRIDGE_PATH = "${cue-bridge}";

          # Shell hook
          shellHook = ''
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
            echo "🚀 Crane-based build system active"
          '';
        };

        # Apps that can be run
        apps = {
          default = flake-utils.lib.mkApp {
            drv = cuenv;
            exePath = "/bin/cuenv";
          };
        };
      });
}
