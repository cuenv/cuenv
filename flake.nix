{
  description = "cuenv - Configuration utilities and validation engine";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";
    crane.url = "github:ipetkov/crane/v0.21.1";
    advisory-db = {
      url = "github:RustSec/advisory-db";
      flake = false;
    };
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

  outputs = { self, nixpkgs, crane, advisory-db, flake-utils, rust-overlay, flake-schemas, ... }:
    let
      systems = [
        builtins.currentSystem
      ];
    in
    flake-utils.lib.eachSystem systems (system:
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

        rustsec-advisory-db = pkgs.runCommand "rustsec-advisory-db-sanitized" {
          src = advisory-db;
          nativeBuildInputs = with pkgs; [ findutils gnugrep ];
        } ''
          mkdir -p "$out"
          cp -R "$src"/. "$out"/
          chmod -R +w "$out"
          while IFS= read -r -d "" file; do
            grep -Ev '^cvss = "CVSS:4\.0/' "$file" > "$file.tmp"
            mv "$file.tmp" "$file"
          done < <(find "$out" -name '*.md' -print0)
        '';

        # CUE bridge builder
        cue-bridge = pkgs.buildGoModule {
          pname = "libcue-bridge";
          inherit version;
          src = ./crates/cuengine;
          vendorHash = "sha256-UD/YJvkzTVVI2gx8LsY8DSKaNIYcDsx+RrtzgryUec8=";
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
              isInContribDir = builtins.match ".*/contrib/.*" path != null || baseName == "contrib";
              # CUE files needed for tests (schema definitions, examples, module config)
              isCueFile = pkgs.lib.hasSuffix ".cue" path;
              isInSchemaDir = builtins.match ".*/schema/.*" path != null || baseName == "schema";
              isInExamplesDir = builtins.match ".*/examples/.*" path != null || baseName == "examples";
              isInCueModDir = builtins.match ".*/cue\\.mod/.*" path != null || baseName == "cue.mod";
              isInTestsDir = builtins.match ".*/_tests/.*" path != null || baseName == "_tests";
              isInFeaturesDir = builtins.match ".*/features/.*" path != null || baseName == "features";
              isLlmsTxt = baseName == "llms.txt";
              isEnvCue = baseName == "env.cue";
              isDenyToml = baseName == "deny.toml";

              # We must include the directories themselves so the filter recurses into them
              isDir = type == "directory";
              isAllowedDir =
                (isInSchemaDir || isInExamplesDir || isInCueModDir || isInTestsDir || isInFeaturesDir || isInContribDir)
                && isDir;
            in
            isCargoSource ||
            isInCratesDir ||
            isInContribDir ||
            isLlmsTxt ||
            isEnvCue ||
            isDenyToml ||
            isAllowedDir ||
            isInTestsDir ||
            isInFeaturesDir ||
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

        # Rust target for cargo-zigbuild (arch-specific, with glibc version)
        zigbuildTarget =
          let cpu = pkgs.stdenv.hostPlatform.parsed.cpu.name;
          in if cpu == "x86_64" then "x86_64-unknown-linux-gnu.2.17"
          else if cpu == "aarch64" then "aarch64-unknown-linux-gnu.2.17"
          else throw "Unsupported Linux architecture: ${cpu}";

        # Build artifacts for dependency caching
        cargoArtifacts = craneLib.buildDepsOnly (commonArgs // {
          src = srcArtifacts;
          preBuild = "";
          buildInputs = platformBuildInputs;
          cargoExtraArgs = "--package cuenv";
        });

        checkArgs = commonArgs // {
          inherit cargoArtifacts;
          doCheck = true;
        };

        clippy-check = craneLib.cargoClippy (checkArgs // {
          cargoExtraArgs = "--locked --workspace --all-features";
          cargoClippyExtraArgs = "--all-targets -- -D warnings";
        });

        nextest-check = craneLib.cargoNextest (checkArgs // {
          cargoExtraArgs = "--locked";
          cargoNextestExtraArgs = "--workspace --all-features";
        });

        doc-test-check = craneLib.cargoDocTest (checkArgs // {
          cargoExtraArgs = "--locked --workspace";
        });

        bdd-check = craneLib.cargoTest (checkArgs // {
          cargoExtraArgs = "--locked";
          cargoTestExtraArgs = "--test bdd";
        });

        deny-check = craneLib.cargoDeny {
          inherit src version;
          pname = "cuenv";
          cargoDenyChecks = "bans licenses";
        };

        audit-check = craneLib.mkCargoDerivation {
          inherit src version;
          pname = "cuenv";
          cargoArtifacts = null;
          cargoVendorDir = null;
          doInstallCargoArtifacts = false;
          buildPhaseCargoCommand = ''
            cargo audit --db ${rustsec-advisory-db} --no-fetch --deny warnings \
              --ignore yanked \
              --ignore RUSTSEC-2023-0071 \
              --ignore RUSTSEC-2025-0057 \
              --ignore RUSTSEC-2025-0134 \
              --ignore RUSTSEC-2026-0006 \
              --ignore RUSTSEC-2026-0020 \
              --ignore RUSTSEC-2026-0021 \
              --ignore RUSTSEC-2026-0037
          '';
          nativeBuildInputs = [ pkgs.cargo-audit ];
        };

        # Main package build
        # On Linux: Use cargo-zigbuild for portable glibc 2.17 binaries
        # On macOS: Use regular cargo with deployment target env var
        cuenv = craneLib.buildPackage (commonArgs // {
          inherit cargoArtifacts;
          pname = "cuenv";
          inherit version;
          doCheck = false; # Tests run via cuenv task check, not nix build
          nativeBuildInputs = commonArgs.nativeBuildInputs ++ [ pkgs.cargo-zigbuild ];
        } // (if pkgs.stdenv.isLinux then {
          # Linux: Use cargo-zigbuild for portable glibc 2.17 binaries
          auditable = false; # cargo-auditable passes --undefined which zig doesn't support
          doNotPostBuildInstallCargoBinaries = true;
          buildPhaseCargoCommand = ''
            export XDG_CACHE_HOME="$TMPDIR/xdg_cache"
            export CARGO_ZIGBUILD_CACHE_DIR="$TMPDIR/zigbuild_cache"
            cargo zigbuild --release --package cuenv --target ${zigbuildTarget}
          '';
          installPhaseCommand = ''
            mkdir -p $out/bin
            cp target/${pkgs.lib.removeSuffix ".2.17" zigbuildTarget}/release/cuenv $out/bin/
          '';
        } else {
          # macOS: Use regular cargo with deployment target set explicitly
          doNotPostBuildInstallCargoBinaries = true;
          buildPhaseCargoCommand = ''
            export MACOSX_DEPLOYMENT_TARGET="11.0"
            cargo build --release --package cuenv
          '';
          installPhaseCommand = ''
            mkdir -p $out/bin
            cp target/release/cuenv $out/bin/

            libiconv_path="$(${pkgs.darwin.cctools}/bin/otool -L $out/bin/cuenv | awk '/libiconv\.2\.dylib/ {print $1; exit}')"
            if [[ -n "$libiconv_path" && "$libiconv_path" == /nix/store/* ]]; then
              ${pkgs.darwin.cctools}/bin/install_name_tool -change "$libiconv_path" /usr/lib/libiconv.2.dylib $out/bin/cuenv
            fi
          '';
        }));

        # Development tools configuration
        devTools = with pkgs; [
          go_1_24
          cue
          antora
          cargo-nextest
          cargo-deny
          cargo-audit
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
        ] ++ lib.optionals stdenv.isLinux [
          cargo-llvm-cov
          gcc   # Provides cc linker for cargo
          patchelf
          libgccjit
          mold  # Fast linker for faster link times
          clang # Required for mold integration
        ];

      in
      {
        checks = {
          inherit cuenv;
          cuenv-audit = audit-check;
          cuenv-bdd = bdd-check;
          cuenv-clippy = clippy-check;
          cuenv-deny = deny-check;
          cuenv-doctest = doc-test-check;
          cuenv-nextest = nextest-check;
        };

        packages = {
          default = cuenv;
          inherit cuenv cue-bridge;
        };

        devShells.default = craneLib.devShell ({
          packages = devTools;

          RUST_BACKTRACE = "1";
          RUST_SRC_PATH = "${rustToolchain}/lib/rustlib/src/rust/library";
          CUE_BRIDGE_PATH = "${cue-bridge}";

          shellHook = ''
            ${setupBridge}

            # sccache configuration — only set if not already provided (e.g. by CI)
            export RUSTC_WRAPPER="''${RUSTC_WRAPPER:-${pkgs.sccache}/bin/sccache}"
            export SCCACHE_DIR="''${SCCACHE_DIR:-$HOME/.cache/sccache}"

            # Install docs dependencies
            cd docs
            bun install
            
            ${pkgs.lib.optionalString pkgs.stdenv.isLinux ''
            # Patch wrangler workerd binary (Linux only)
            __patchTarget="./node_modules/@cloudflare/workerd-linux-64/bin/workerd"
            if [[ -f "$__patchTarget" ]]; then
              ${pkgs.patchelf}/bin/patchelf --set-interpreter ${pkgs.glibc}/lib/ld-linux-x86-64.so.2 "$__patchTarget"
            fi

            # Use clang+mold linker for faster linking (local dev only).
            # In CI, clang can't handle LTO objects from some crates (alloca).
            if [ -z "''${CI:-}" ]; then
              export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER="clang"
              export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS="-C link-arg=-fuse-ld=mold"
              export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER="clang"
              export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS="-C link-arg=-fuse-ld=mold"
            fi
            ''}

            cd ..

            echo "cuenv development environment ready!"
            echo "Prebuilt CUE bridge available at: ${cue-bridge}"
            echo "Crane-based build system active"
            echo "sccache enabled (RUSTC_WRAPPER set)"
            echo "Docs dependencies installed${pkgs.lib.optionalString pkgs.stdenv.isLinux ", wrangler patched, mold linker available"}"
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
