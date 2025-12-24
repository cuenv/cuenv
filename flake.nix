{
  description = "cuenv - Configuration utilities and validation engine";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";
    crane.url = "github:ipetkov/crane/v0.21.1";
    flake-utils.url = "github:numtide/flake-utils/v1.0.0";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    flake-schemas.url = "https://flakehub.com/f/DeterminateSystems/flake-schemas/0.2.0";
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

  outputs = { self, nixpkgs, crane, flake-utils, rust-overlay, flake-schemas, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };

        rustToolchain = pkgs.rust-bin.stable."1.90.0".default.override {
          extensions = [ "rust-src" "rust-analyzer" "clippy" "rustfmt" "llvm-tools-preview" ];
          targets = [
            "x86_64-unknown-linux-gnu"
            "aarch64-unknown-linux-gnu"
            "aarch64-apple-darwin"
            "x86_64-apple-darwin"
          ];
        };

        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

        # Read version from Cargo.toml
        cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
        version = cargoToml.workspace.package.version;

        # Zig target for C compilation (used by CGO)
        zigTarget =
          let
            cpu = pkgs.stdenv.hostPlatform.parsed.cpu.name;
            os = pkgs.stdenv.hostPlatform.parsed.kernel.name;
          in
          if os == "linux" then
            if cpu == "x86_64" then "x86_64-linux-gnu.2.17"
            else if cpu == "aarch64" then "aarch64-linux-gnu.2.17"
            else throw "Unsupported Linux architecture: ${cpu}"
          else if os == "darwin" then
            if cpu == "aarch64" then "aarch64-macos.11.0"
            else if cpu == "x86_64" then "x86_64-macos.11.0"
            else throw "Unsupported macOS architecture: ${cpu}"
          else throw "Unsupported OS: ${os}";


        # Zig wrappers for CGO (CC cannot contain spaces)
        zigCCWrapper = pkgs.writeShellScriptBin "zig-cc" ''
          exec ${pkgs.zig}/bin/zig cc -target ${zigTarget} "$@"
        '';
        zigCXXWrapper = pkgs.writeShellScriptBin "zig-cxx" ''
          exec ${pkgs.zig}/bin/zig c++ -target ${zigTarget} "$@"
        '';
        zigARWrapper = pkgs.writeShellScriptBin "zig-ar" ''
          exec ${pkgs.zig}/bin/zig ar "$@"
        '';

        # Platform-specific build inputs
        # Note: darwin frameworks (CoreFoundation, Security, etc.) are now provided
        # automatically by the default SDK - no explicit references needed
        platformBuildInputs = with pkgs;
          [ libiconv ];

        # 1Password WASM SDK (fetched for tests)
        onepassword-wasm = pkgs.fetchurl {
          url = "https://github.com/1Password/onepassword-sdk-go/raw/refs/tags/v0.3.1/internal/wasm/core.wasm";
          hash = "sha256-hY3SBC679vUNDkpREjfUWAaQxC5mrPQhdYSuUKx+j2o=";
        };

        # CUE bridge builder
        cue-bridge = pkgs.buildGoModule {
          pname = "libcue-bridge";
          inherit version;
          src = ./crates/cuengine;
          vendorHash = "sha256-tHAcwRsNWNwPUkTlQT8mw3GNKsMFCMCKwdSq3KNad80=";
          go = pkgs.go_1_24;
          nativeBuildInputs = [ pkgs.zig zigCCWrapper zigCXXWrapper zigARWrapper ]
            ++ pkgs.lib.optionals (!pkgs.stdenv.isDarwin) [ pkgs.binutils ];

          buildPhase = ''
            runHook preBuild

            export CGO_ENABLED=1
            export GOOS=${pkgs.stdenv.hostPlatform.parsed.kernel.name}
            export GOARCH=${
              let cpu = pkgs.stdenv.hostPlatform.parsed.cpu.name;
              in if cpu == "x86_64" then "amd64"
              else if cpu == "aarch64" then "arm64"
              else cpu
            }
            export CC=${zigCCWrapper}/bin/zig-cc
            export CXX=${zigCXXWrapper}/bin/zig-cxx
            export AR=${zigARWrapper}/bin/zig-ar

            # Zig needs writable cache directories in Nix sandbox
            export ZIG_GLOBAL_CACHE_DIR="$TMPDIR/zig-cache"
            export ZIG_LOCAL_CACHE_DIR="$TMPDIR/zig-local-cache"

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
              isLlmsTxt = baseName == "llms.txt";
              isEnvCue = baseName == "env.cue";

              # We must include the directories themselves so the filter recurses into them
              isDir = type == "directory";
              isAllowedDir = (isInSchemaDir || isInExamplesDir || isInCueModDir) && isDir;
            in
            isCargoSource ||
            isInCratesDir ||
            isLlmsTxt ||
            isEnvCue ||
            isAllowedDir ||
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
          mkdir -p target/debug target/release

          rm -f target/debug/libcue_bridge.*
          rm -f target/release/libcue_bridge.*

          cp -r ${cue-bridge}/debug/* target/debug/
          cp -r ${cue-bridge}/release/* target/release/

          chmod -R +w target
        '';


        # Common build configuration
        commonArgs = {
          inherit src;
          strictDeps = true;
          nativeBuildInputs = with pkgs; [ go pkg-config cue git zig ];
          buildInputs = platformBuildInputs;
          preBuild = ''
            ${setupBridge}
          '';
          CUE_BRIDGE_PATH = cue-bridge;
          ONEPASSWORD_WASM_PATH = onepassword-wasm;
        };

        # Build artifacts for dependency caching
        cargoArtifacts = craneLib.buildDepsOnly (commonArgs // {
          src = srcArtifacts;
          preBuild = "";
          buildInputs = platformBuildInputs;
        });

        # Main package build
        # On Linux: Use Zig as CC/linker for portable binaries (glibc 2.17 target)
        # On macOS: Use regular cargo with deployment target env var
        cuenv = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;
          pname = "cuenv";
          inherit version;
        } // (if pkgs.stdenv.isLinux then {
          # Linux: Use Zig wrappers for portable glibc binaries
          doNotPostBuildInstallCargoBinaries = true;
          buildPhaseCargoCommand = ''
            export ZIG_GLOBAL_CACHE_DIR="$TMPDIR/zig-cache"
            export ZIG_LOCAL_CACHE_DIR="$TMPDIR/zig-local-cache"
            export CC=${zigCCWrapper}/bin/zig-cc
            export CXX=${zigCXXWrapper}/bin/zig-cxx
            export AR=${zigARWrapper}/bin/zig-ar
            export RUSTFLAGS="-C linker=${zigCCWrapper}/bin/zig-cc"
            cargo build --release
          '';
          installPhaseCommand = ''
            mkdir -p $out/bin
            cp target/release/cuenv $out/bin/
          '';
        } else {
          # macOS: Use regular cargo with deployment target set explicitly
          doNotPostBuildInstallCargoBinaries = true;
          buildPhaseCargoCommand = ''
            export MACOSX_DEPLOYMENT_TARGET="11.0"
            cargo build --release
          '';
          installPhaseCommand = ''
            mkdir -p $out/bin
            cp target/release/cuenv $out/bin/
          '';
        }));

        # Development tools configuration
        devTools = with pkgs; [
          go_1_24
          cue
          antora
          cargo-nextest
          cargo-deny
          cargo-cyclonedx
          zig
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
          inherit cuenv cue-bridge;
        };

        checks = {
          inherit cuenv;

          cuenv-fmt = pkgs.runCommand "cuenv-fmt-${version}"
            {
              nativeBuildInputs = with pkgs; [ treefmt rustfmt go nodePackages.prettier nixpkgs-fmt ];
            } ''
            cp -r ${src} src
            chmod -R +w src
            cd src
            cp ${./treefmt.toml} treefmt.toml
            treefmt --no-cache --fail-on-change
            touch $out
          '';

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
