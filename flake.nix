{
  description = "cuenv - Configuration utilities and validation engine";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.05";
    nixpkgs-unstable.url = "github:NixOS/nixpkgs/nixos-unstable";
    crane.url = "github:ipetkov/crane";
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

  nixConfig = {
    extra-substituters = [
      "https://cache.nixos.org"
      "https://cuenv.cachix.org"
    ];
    extra-trusted-public-keys = [
      "cuenv.cachix.org-1:zPi7E3HNNHEYzsDwSMGXk0pvEeWzdrb/09B/JozulHw="
    ];
    experimental-features = [ "nix-command" "flakes" ];
    accept-flake-config = true;
  };

  outputs = { self, nixpkgs, nixpkgs-unstable, crane, flake-utils, rust-overlay, advisory-db, flake-schemas, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };

        pkgs-unstable = import nixpkgs-unstable {
          inherit system;
        };

        rustToolchain = pkgs.rust-bin.stable."1.90.0".default.override {
          extensions = [ "rust-src" "rust-analyzer" "clippy" "rustfmt" "llvm-tools-preview" ];
          targets = [ "x86_64-unknown-linux-musl" ];
        };

        # Use pkgsCross.musl64 for static builds to ensure we target Linux Musl
        # correctly even when building from macOS (cross-compilation).
        # On Linux, this is effectively equivalent to pkgsStatic for Musl.
        pkgsStatic = pkgs.pkgsCross.musl64;

        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;
        craneLibStatic = (crane.mkLib pkgsStatic).overrideToolchain rustToolchain;

        # Platform-specific build inputs
        darwinFrameworks = with pkgs.darwin.apple_sdk.frameworks; [
          CoreFoundation
          Security
          SystemConfiguration
        ];

        platformBuildInputs = with pkgs;
          [ libiconv ] ++ lib.optionals stdenv.isDarwin darwinFrameworks;

        # CUE bridge builder
        mkCueBridge = pkgs: pkgs.buildGoModule {
          pname = "libcue-bridge";
          version = "0.1.1";
          src = ./crates/cuengine;
          vendorHash = "sha256-tHAcwRsNWNwPUkTlQT8mw3GNKsMFCMCKwdSq3KNad80=";
          go = pkgs.go_1_24;
          nativeBuildInputs = pkgs.lib.optionals (!pkgs.stdenv.isDarwin) [ pkgs.binutils ];
          buildInputs = pkgs.lib.optionals pkgs.stdenv.isDarwin darwinFrameworks;

          buildPhase = ''
            runHook preBuild

            # Force CGO to use the *target* toolchain so the archive matches the Rust target
            export CGO_ENABLED=1
            export GOOS=${pkgs.stdenv.hostPlatform.parsed.kernel.name}
            export GOARCH=${
              let cpu = pkgs.stdenv.hostPlatform.parsed.cpu.name;
              in if cpu == "x86_64" then "amd64"
              else if cpu == "aarch64" then "arm64"
              else cpu
            }
            export CC=${pkgs.stdenv.cc}/bin/${pkgs.stdenv.cc.targetPrefix}cc
            export CXX=${pkgs.stdenv.cc}/bin/${pkgs.stdenv.cc.targetPrefix}c++
            export AR=${pkgs.stdenv.cc.bintools.bintools_bin}/bin/${pkgs.stdenv.cc.targetPrefix}ar
            export RANLIB=${pkgs.stdenv.cc.bintools.bintools_bin}/bin/${pkgs.stdenv.cc.targetPrefix}ranlib
            export PKG_CONFIG_ALLOW_CROSS=1
            ${pkgs.lib.optionalString pkgs.stdenv.targetPlatform.isMusl ''
            export CGO_CFLAGS="-static"
            export CGO_LDFLAGS="-static"
            ''}

            mkdir -p $out/debug $out/release

            # For musl targets, add -linkmode external and -extldflags '-static' to ensure fully static linking
            # This fixes segfaults with c-archive buildmode on musl (see: https://github.com/golang/go/pull/69325)
            ${pkgs.lib.optionalString pkgs.stdenv.targetPlatform.isMusl ''
            go build -buildmode=c-archive -ldflags "-linkmode external -extldflags '-static'" -o $out/debug/libcue_bridge.a bridge.go
            cp libcue_bridge.h $out/debug/
            
            CGO_ENABLED=1 go build -ldflags="-s -w -linkmode external -extldflags '-static'" -buildmode=c-archive -o $out/release/libcue_bridge.a bridge.go
            cp libcue_bridge.h $out/release/
            ''}
            
            # For non-musl targets, build normally without static flags
            ${pkgs.lib.optionalString (!pkgs.stdenv.targetPlatform.isMusl) ''
            go build -buildmode=c-archive -o $out/debug/libcue_bridge.a bridge.go
            cp libcue_bridge.h $out/debug/
            
            CGO_ENABLED=1 go build -ldflags="-s -w" -buildmode=c-archive -o $out/release/libcue_bridge.a bridge.go
            cp libcue_bridge.h $out/release/
            ''}
            
            runHook postBuild
          '';

          installPhase = ''
            runHook preInstall
            runHook postInstall
          '';

          meta = with pkgs.lib; {
            description = "Go CUE bridge library for cuenv";
            license = with licenses; [ mit asl20 ];
            platforms = platforms.unix ++ platforms.windows;
          };
        };

        cue-bridge = mkCueBridge pkgs;

        cue-bridge-static = mkCueBridge pkgsStatic;

        # Source filtering for Rust builds
        # Uses Crane's filterCargoSources for proper cache invalidation
        # See: https://crane.dev/faq/constant-rebuilds.html
        src = pkgs.lib.cleanSourceWith {
          src = ./.;
          filter = path: type:
            let
              baseName = builtins.baseNameOf path;
              # Use Crane's built-in filter for Rust/Cargo files (.rs, .toml, Cargo.lock)
              isCargoSource = craneLib.filterCargoSources path type;
              # Include all files in crates/ (Rust code, Go bridge, test fixtures)
              isInCratesDir = builtins.match ".*/crates/.*" path != null || baseName == "crates";
              # CUE files needed for tests (schema definitions, examples, module config)
              isCueFile = pkgs.lib.hasSuffix ".cue" path;
              isInSchemaDir = builtins.match ".*/schema/.*" path != null || baseName == "schema";
              isInExamplesDir = builtins.match ".*/examples/.*" path != null || baseName == "examples";
              isInCueModDir = builtins.match ".*/cue\\.mod/.*" path != null || baseName == "cue.mod";
            in
            isCargoSource ||
            isInCratesDir ||
            ((isInSchemaDir || isInExamplesDir || isInCueModDir) && isCueFile);
        };

        # Strict source filtering for dependencies (Cargo.toml/lock only)
        # This ensures cargoArtifacts are cached even when source code changes
        srcArtifacts = pkgs.lib.cleanSourceWith {
          src = ./.;
          filter = path: type:
            let
              baseName = builtins.baseNameOf path;
              isCargoMetadata = baseName == "Cargo.toml" || baseName == "Cargo.lock";
              isCargoConfig = baseName == "config.toml" && builtins.match ".*/\\.cargo/.*" path != null;
            in
            isCargoMetadata || isCargoConfig;
        };

        # Bridge setup helper
        setupBridge = ''
          mkdir -p crates/core/src/target/debug crates/core/src/target/release
          
          # Remove existing files to avoid permission issues with read-only nix store files
          rm -f crates/core/src/target/debug/libcue_bridge.*
          rm -f crates/core/src/target/release/libcue_bridge.*

          cp -r ${cue-bridge}/debug/* crates/core/src/target/debug/
          cp -r ${cue-bridge}/release/* crates/core/src/target/release/
          
          # Ensure the copied files are writable
          chmod -R +w crates/core/src/target
        '';

        setupBridgeStatic = ''
          mkdir -p crates/core/src/target/debug crates/core/src/target/release
          
          # Remove existing files to avoid permission issues with read-only nix store files
          rm -f crates/core/src/target/debug/libcue_bridge.*
          rm -f crates/core/src/target/release/libcue_bridge.*

          cp -r ${cue-bridge-static}/debug/* crates/core/src/target/debug/
          cp -r ${cue-bridge-static}/release/* crates/core/src/target/release/
          
          # Ensure the copied files are writable
          chmod -R +w crates/core/src/target
        '';


        # Common build configuration
        commonArgs = {
          inherit src;
          strictDeps = true;
          nativeBuildInputs = with pkgs; [ go pkg-config pkgs-unstable.cue git ];
          buildInputs = platformBuildInputs;
          preBuild = ''
            ${setupBridge}
          '';
          CUE_BRIDGE_PATH = cue-bridge;
        };

        commonArgsStatic = commonArgs // {
          CUE_BRIDGE_PATH = cue-bridge-static;
          preBuild = setupBridgeStatic;
          CARGO_BUILD_TARGET = "x86_64-unknown-linux-musl";
          # Ensure we link libc statically and don't leak nix-store rpaths
          RUSTFLAGS = "-C target-feature=+crt-static";
          CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_RUSTFLAGS = "-C target-feature=+crt-static";
          CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER =
            "${pkgsStatic.stdenv.cc}/bin/${pkgsStatic.stdenv.cc.targetPrefix}cc";
          # Override buildInputs to use static libs if needed, or empty if none
          buildInputs = [ ];
        };

        # Build artifacts for dependency caching
        cargoArtifacts = craneLib.buildDepsOnly (commonArgs // {
          src = srcArtifacts;
          preBuild = ""; # No bridge needed for deps
          buildInputs = platformBuildInputs;
        });

        cargoArtifactsStatic = craneLibStatic.buildDepsOnly (commonArgsStatic // {
          src = srcArtifacts;
          preBuild = "";
        });

        # Main package build
        cuenv = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;
          pname = "cuenv";
          version = "0.1.1";
        });

        cuenv-static = craneLibStatic.buildPackage (commonArgsStatic // {
          inherit cargoArtifactsStatic;
          pname = "cuenv";
          version = "0.1.1";
          doCheck = false; # Skip tests due to Go CGO + musl runtime incompatibility
        });

        # Development tools configuration
        devTools = with pkgs; [
          go_1_24
          pkgs-unstable.cue # Use CUE from unstable for latest version
          antora
          cargo-audit
          cargo-nextest
          cargo-deny
          cargo-cyclonedx
          git
          gh
          jq
          nodePackages.prettier
          nixpkgs-fmt
          treefmt
          pkg-config
          llvmPackages.bintools
          bun
          sccache
        ] ++ lib.optionals stdenv.isLinux [
          cargo-llvm-cov
          patchelf
          libgccjit
        ];

      in
      {
        packages = {
          default = cuenv;
          inherit cuenv cuenv-static cue-bridge;
        };

        checks = {
          inherit cuenv;

          cuenv-fmt = craneLib.cargoFmt { inherit src; };

          cuenv-clippy = craneLib.cargoClippy (commonArgs // {
            inherit cargoArtifacts;
            cargoClippyExtraArgs = "--all-targets -- -D warnings";
          });

          cuenv-doc = craneLib.cargoDoc (commonArgs // {
            inherit cargoArtifacts;
          });

          cuenv-test = craneLib.cargoNextest (commonArgs // {
            inherit cargoArtifacts;
            partitions = 1;
            partitionType = "count";
          });

          cuenv-audit = craneLib.cargoAudit {
            inherit src advisory-db;
          };

          cuenv-deny = craneLib.cargoDeny {
            inherit src;
          };
        };

        devShells.default = craneLib.devShell ({
          checks = self.checks.${system};
          packages = devTools;

          RUST_BACKTRACE = "1";
          RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";
          CUE_BRIDGE_PATH = "${cue-bridge}";

          shellHook = ''
            ${setupBridge}
            
            # Install docs dependencies
            cd docs
            bun install
            
            ${pkgs.lib.optionalString pkgs.stdenv.isLinux ''
            # Patch wrangler workerd binary (Linux only)
            __patchTarget="./node_modules/@cloudflare/workerd-linux-64/bin/workerd"
            if [[ -f "$__patchTarget" ]]; then
              ${pkgs.patchelf}/bin/patchelf --set-interpreter ${pkgs.glibc}/lib/ld-linux-x86-64.so.2 "$__patchTarget"
            fi
            ''}
            
            cd ..
            
            # Run cargo audit to check for vulnerabilities, ignoring specific advisory
            cargo audit --ignore RUSTSEC-2024-0436
            
            echo "🦀 cuenv development environment ready!"
            echo "📦 Prebuilt CUE bridge available at: ${cue-bridge}"
            echo "🚀 Crane-based build system active"
            echo "📚 Docs dependencies installed${pkgs.lib.optionalString pkgs.stdenv.isLinux " and wrangler patched"}"
          '';
        } // pkgs.lib.optionalAttrs pkgs.stdenv.isLinux {
          LD_LIBRARY_PATH = "${pkgs.libgccjit}/lib:$LD_LIBRARY_PATH";
        });

        apps = {
          default = {
            type = "app";
            program = "${cuenv}/bin/cuenv";
            meta = {
              description = "cuenv CLI app";
            };
          };
        };
      });
}
