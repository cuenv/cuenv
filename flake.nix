{
  description = "cuenv - Configuration utilities and validation engine";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.05";
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

        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" "clippy" "rustfmt" "llvm-tools-preview" ];
        };

        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

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
          version = "0.1.0";
          src = ./crates/cuengine;
          vendorHash = "sha256-mU40RCeO0R286fxfgONJ7kw6kFDHPMUzHw8sjsBgiRg";
          buildInputs = pkgs.lib.optionals pkgs.stdenv.isDarwin darwinFrameworks;

          buildPhase = ''
            runHook preBuild
            
            mkdir -p $out/debug $out/release
            
            go build -buildmode=c-archive -o $out/debug/libcue_bridge.a bridge.go
            cp libcue_bridge.h $out/debug/
            
            CGO_ENABLED=1 go build -ldflags="-s -w" -buildmode=c-archive -o $out/release/libcue_bridge.a bridge.go
            cp libcue_bridge.h $out/release/
            
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

        # Source filtering for Rust builds
        src = pkgs.lib.cleanSourceWith {
          src = ./.;
          filter = path: type:
            let
              isRustFile = pkgs.lib.hasSuffix "\.rs" path;
              isTomlFile = pkgs.lib.hasSuffix "\.toml" path;
              isLockFile = pkgs.lib.hasSuffix "\.lock" path;
              isGoFile = pkgs.lib.hasSuffix "\.go" path ||
                pkgs.lib.hasSuffix "\.mod" path ||
                pkgs.lib.hasSuffix "\.sum" path;
              isBridgeHeader = pkgs.lib.hasSuffix "bridge.h" path;
              isCueFile = pkgs.lib.hasSuffix "\.cue" path;
              isDirectory = type == "directory";
            in
            isRustFile || isTomlFile || isLockFile ||
            isGoFile || isBridgeHeader || isCueFile || isDirectory;
        };

        # Bridge setup helper
        setupBridge = ''
          mkdir -p target/debug target/release
          cp -r ${cue-bridge}/debug/* target/debug/ || true
          cp -r ${cue-bridge}/release/* target/release/ || true
        '';

        # Common build configuration
        commonArgs = {
          inherit src;
          strictDeps = true;
          nativeBuildInputs = with pkgs; [ go pkg-config ];
          buildInputs = platformBuildInputs;
          preBuild = setupBridge;
          CUE_BRIDGE_PATH = cue-bridge;
        };

        # Build artifacts for dependency caching
        cargoArtifacts = craneLib.buildDepsOnly (commonArgs // {
          buildInputs = platformBuildInputs;
        });

        # Main package build
        cuenv = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;
          pname = "cuenv";
          version = "0.1.0";
        });

        # Individual crate builder helper
        mkCrate = { pname, cargoExtraArgs }: craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts pname cargoExtraArgs;
          doCheck = false;
        });

        # Development tools configuration
        devTools = with pkgs; [
          go_1_24
          cue
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
        ] ++ lib.optionals stdenv.isLinux [
          cargo-llvm-cov
          patchelf
          libgccjit
        ];

      in
      {
        schemas = flake-schemas.schemas;

        packages = {
          default = cuenv;
          inherit cuenv cue-bridge;
          cuenv-cli = mkCrate {
            pname = "cuenv-cli";
            cargoExtraArgs = "-p cuenv-cli";
          };
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

          cuenv-nextest = craneLib.cargoNextest (commonArgs // {
            inherit cargoArtifacts;
            partitions = 1;
            partitionType = "count";
          });

          cuenv-audit = craneLib.cargoAudit {
            inherit src advisory-db;
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
            
            echo "🦀 cuenv development environment ready!"
            echo "📦 Prebuilt CUE bridge available at: ${cue-bridge}"
            echo "🚀 Crane-based build system active"
            echo "📚 Docs dependencies installed${pkgs.lib.optionalString pkgs.stdenv.isLinux " and wrangler patched"}"
          '';
        } // pkgs.lib.optionalAttrs pkgs.stdenv.isLinux {
          LD_LIBRARY_PATH = "${pkgs.libgccjit}/lib:$LD_LIBRARY_PATH";
        });

        apps = {
          default = flake-utils.lib.mkApp {
            drv = cuenv;
            exePath = "/bin/cuenv";
          };
        };
      });
}
