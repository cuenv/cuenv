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
    ];
    experimental-features = [ "nix-command" "flakes" ];
    accept-flake-config = true;
  };

  outputs = { self, nixpkgs, crane, advisory-db, flake-utils, rust-overlay, flake-schemas, ... }:
    let
      systems = [
        "aarch64-darwin"
        "aarch64-linux"
        "x86_64-darwin"
        "x86_64-linux"
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

        # CGO toolchain shared by the CUE bridge's build and check phases
        cgoToolchainSetup = ''
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
        '';

        # CUE bridge builder
        cue-bridge = pkgs.buildGoModule {
          pname = "libcue-bridge";
          inherit version;
          src = ./crates/cuengine;
          vendorHash = "sha256-p8gfl2H0lThSmqIRQZWDYoQ3antrIslpCwRCNKQ1cKs=";
          go = pkgs.go_1_24;
          nativeBuildInputs = [ pkgs.zig zigCCWrapper zigCXXWrapper zigARWrapper ]
            ++ pkgs.lib.optionals (!pkgs.stdenv.isDarwin) [ pkgs.binutils ];

          # buildGoModule's default checkPhase calls `getGoDirs`, which only its own
          # buildPhase defines. Overriding buildPhase therefore made the inherited
          # checkPhase fail silently and skip every Go test, so the check phase is
          # explicit and shares the CGO toolchain setup with the build.
          buildPhase = ''
            runHook preBuild

            ${cgoToolchainSetup}

            mkdir -p $out/debug $out/release
            go_sources=$(find . -maxdepth 1 -name '*.go' ! -name '*_test.go' -print | sort)

            go build -buildmode=c-archive -o $out/debug/libcue_bridge.a $go_sources
            cp libcue_bridge.h $out/debug/

            CGO_ENABLED=1 go build -ldflags="-s -w" -buildmode=c-archive -o $out/release/libcue_bridge.a $go_sources
            cp libcue_bridge.h $out/release/

            runHook postBuild
          '';

          doCheck = true;
          # Tests use the stdenv compiler wrapper rather than zig: the test
          # binaries run here, and zig links them against a dynamic loader
          # path that does not exist inside the Nix sandbox. Zig is only
          # needed for the portable archives the build phase produces.
          checkPhase = ''
            runHook preCheck

            # buildPhase exported the zig wrappers into this shell; replace them.
            export CGO_ENABLED=1
            export CC=${pkgs.stdenv.cc}/bin/cc
            export CXX=${pkgs.stdenv.cc}/bin/c++
            unset AR

            go vet ./...
            go test ./...

            runHook postCheck
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
              isAgentsRootDir = baseName == ".agents";
              isAgentsSkillsRoot = builtins.match ".*/\\.agents/skills" path != null;
              isInAgentsSkillsDir = builtins.match ".*/\\.agents/skills/.*" path != null;
              isLlmsTxt = baseName == "llms.txt";
              isEnvCue = baseName == "env.cue";
              isDenyToml = baseName == "deny.toml";

              # We must include the directories themselves so the filter recurses into them
              isDir = type == "directory";
              isAllowedDir =
                (
                  isInSchemaDir
                  || isInExamplesDir
                  || isInCueModDir
                  || isInTestsDir
                  || isInContribDir
                  || isAgentsRootDir
                  || isAgentsSkillsRoot
                  || isInAgentsSkillsDir
                )
                && isDir;
            in
            isCargoSource ||
            isInCratesDir ||
            isInContribDir ||
            isInAgentsSkillsDir ||
            isLlmsTxt ||
            isEnvCue ||
            isDenyToml ||
            isAllowedDir ||
            isInTestsDir ||
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

        lockedCargoExtraArgs = "--locked";
        workspaceAllFeaturesCargoExtraArgs = "${lockedCargoExtraArgs} --workspace --all-features";
        workspaceDocCargoExtraArgs = "${lockedCargoExtraArgs} --workspace";
        cuenvPackageCargoExtraArgs = "${lockedCargoExtraArgs} --package cuenv";

        # Keep dependency-only derivations aligned with downstream cargo flags.
        # Mixing workspace-wide and package-specific scopes forces Cargo to rebuild.
        cargoArtifactsArgs = {
          src = srcArtifacts;
          preBuild = "";
          buildInputs = platformBuildInputs;
        };

        workspaceCargoArtifacts = craneLib.buildDepsOnly (commonArgs // cargoArtifactsArgs // {
          cargoExtraArgs = workspaceAllFeaturesCargoExtraArgs;
        });

        # Test binaries are built with the lightweight `ci` profile (no LTO,
        # default codegen-units) so the workspace-wide nextest compile stays
        # within CI runner memory. The release binary keeps `[profile.release]`.
        workspaceCiCargoArtifacts = craneLib.buildDepsOnly (commonArgs // cargoArtifactsArgs // {
          cargoExtraArgs = workspaceAllFeaturesCargoExtraArgs;
          CARGO_PROFILE = "ci";
        });

        workspaceDocCargoArtifacts = craneLib.buildDepsOnly (commonArgs // cargoArtifactsArgs // {
          cargoExtraArgs = workspaceDocCargoExtraArgs;
        });

        cuenvCargoArtifacts = craneLib.buildDepsOnly (commonArgs // cargoArtifactsArgs // {
          cargoExtraArgs = cuenvPackageCargoExtraArgs;
        });

        workspaceCheckArgs = commonArgs // {
          cargoArtifacts = workspaceCargoArtifacts;
          doCheck = true;
        };

        workspaceDocCheckArgs = commonArgs // {
          cargoArtifacts = workspaceDocCargoArtifacts;
          doCheck = true;
        };

        cuenvCheckArgs = commonArgs // {
          cargoArtifacts = cuenvCargoArtifacts;
          doCheck = true;
        };

        clippy-check = craneLib.cargoClippy (workspaceCheckArgs // {
          cargoExtraArgs = workspaceAllFeaturesCargoExtraArgs;
          cargoClippyExtraArgs = "--all-targets -- -D warnings";
        });

        nextest-check = craneLib.cargoNextest (workspaceCheckArgs // {
          cargoArtifacts = workspaceCiCargoArtifacts;
          cargoExtraArgs = workspaceAllFeaturesCargoExtraArgs;
          CARGO_PROFILE = "ci";
        });

        # Fake Terraform provider used by the infrastructure end-to-end suites. It
        # reproduces provider behaviours real providers show only occasionally;
        # `go vet` runs as its check phase because the source is not part of the
        # cuengine Go module.
        fake-terraform-provider = pkgs.buildGoModule {
          pname = "terraform-provider-fake";
          version = "0.0.0";
          src = ./crates/infrastructure/tests/fake_provider;
          vendorHash = "sha256-HM6/k59eDfD7OpH5S3C8j6YN0Q0ina6KuKfrJVWyTB8=";
          doCheck = true;
          checkPhase = ''
            runHook preCheck
            go vet ./...
            runHook postCheck
          '';
          meta = {
            description = "Fake Terraform provider for cuenv infrastructure tests";
            mainProgram = "terraform-provider-fake";
          };
        };

        # The ignored suites need a libSQL server and real provider binaries, so
        # they are skipped by `cuenv-nextest`. This check provides them inside the
        # Nix sandbox (loopback only, no network): sqld on free loopback ports, the
        # fake provider and the hashicorp random, local and tfe providers from
        # nixpkgs. `installs_provider_from_registry` is excluded because it
        # downloads from registry.terraform.io. Only ignored tests run here; the
        # rest of each crate's tests belong to `cuenv-nextest`.
        #
        # Only the crates and targets the suites live in are built, without debug
        # information, with their own dependency artifacts: the workspace-wide
        # artifacts of `cuenv-nextest` would compile every test binary again.
        infrastructureE2eCargoExtraArgs = builtins.concatStringsSep " " [
          lockedCargoExtraArgs
          "--package cuenv-infrastructure"
          "--package cuenv"
        ];
        infrastructureE2eBuildArgs = {
          CARGO_PROFILE = "ci";
          CARGO_PROFILE_CI_DEBUG = "0";
        };

        infrastructureE2eCargoArtifacts = craneLib.buildDepsOnly (commonArgs // cargoArtifactsArgs // infrastructureE2eBuildArgs // {
          pname = "cuenv-infrastructure-e2e";
          cargoExtraArgs = infrastructureE2eCargoExtraArgs;
        });

        infrastructure-e2e-check = craneLib.cargoNextest (workspaceCheckArgs // infrastructureE2eBuildArgs // {
          pname = "cuenv-infrastructure-e2e";
          cargoArtifacts = infrastructureE2eCargoArtifacts;
          cargoExtraArgs = builtins.concatStringsSep " " [
            infrastructureE2eCargoExtraArgs
            "--lib"
            "--test provider_end_to_end"
            "--test infrastructure_lifecycle"
          ];
          nativeBuildInputs = commonArgs.nativeBuildInputs ++ [
            pkgs.curl
            pkgs.sqld
            fake-terraform-provider
            pkgs.terraform-providers.hashicorp_random
            pkgs.terraform-providers.hashicorp_local
            pkgs.terraform-providers.hashicorp_tfe
          ];
          cargoNextestExtraArgs = builtins.concatStringsSep " " [
            "--run-ignored ignored-only"
            "--no-fail-fast"
            "--retries 0"
            "--status-level pass"
            "--hide-progress-bar"
            "-E"
            "'(package(cuenv-infrastructure) | (package(cuenv) & binary(infrastructure_lifecycle))) & not test(installs_provider_from_registry)'"
          ];
          preCheck = ''
            export HOME="$TMPDIR/home"
            mkdir -p "$HOME"

            # sqld loads the platform's root certificates at startup, even for a
            # plain-HTTP listener, and panics when it finds none.
            export SSL_CERT_FILE="${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"

            provider_binary() {
              find "$1" -type f -name "terraform-provider-$2*" | sort | head -n 1
            }
            export CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER="${fake-terraform-provider}/bin/terraform-provider-fake"
            export CUENV_INFRASTRUCTURE_TEST_RANDOM_PROVIDER="$(provider_binary ${pkgs.terraform-providers.hashicorp_random} random)"
            export CUENV_INFRASTRUCTURE_TEST_LOCAL_PROVIDER="$(provider_binary ${pkgs.terraform-providers.hashicorp_local} local)"
            export CUENV_INFRASTRUCTURE_TEST_TFE_PROVIDER="$(provider_binary ${pkgs.terraform-providers.hashicorp_tfe} tfe)"
            for variable in FAKE RANDOM LOCAL TFE; do
              name="CUENV_INFRASTRUCTURE_TEST_''${variable}_PROVIDER"
              if [ ! -x "''${!name}" ]; then
                echo "$name does not point at an executable: ''${!name}" >&2
                exit 1
              fi
            done

            # Ports are chosen by probing loopback so the check also works when the
            # sandbox is unavailable and the host network is shared.
            free_port() {
              local candidate
              for _ in $(seq 1 100); do
                candidate=$((20000 + RANDOM % 40000))
                if ! (exec 3<>"/dev/tcp/127.0.0.1/$candidate") 2>/dev/null; then
                  echo "$candidate"
                  return 0
                fi
              done
              echo "no free loopback port" >&2
              return 1
            }

            sqld_pids=""
            stop_sqld() {
              for pid in $sqld_pids; do
                kill "$pid" 2>/dev/null || true
              done
            }
            trap stop_sqld EXIT

            # Each libSQL database that a suite requires to be otherwise unused gets
            # its own server; the shared one serves everything else.
            start_sqld() {
              local port
              port="$(free_port)"
              mkdir -p "$TMPDIR/sqld-$1"
              sqld --http-listen-addr "127.0.0.1:$port" --db-path "$TMPDIR/sqld-$1/data.sqld" \
                > "$TMPDIR/sqld-$1.log" 2>&1 &
              sqld_pids="$sqld_pids $!"
              for _ in $(seq 1 100); do
                if curl --silent --fail "http://127.0.0.1:$port/health" > /dev/null; then
                  started_sqld_url="http://127.0.0.1:$port"
                  return 0
                fi
                sleep 0.2
              done
              echo "sqld ($1) did not become healthy" >&2
              cat "$TMPDIR/sqld-$1.log" >&2
              return 1
            }
            # start_sqld runs in this shell (not a command substitution) so that the
            # server's pid is recorded for the EXIT trap.
            start_sqld shared
            export CUENV_INFRASTRUCTURE_TEST_TURSO_URL="$started_sqld_url"
            start_sqld fresh
            export CUENV_INFRASTRUCTURE_TEST_FRESH_TURSO_URL="$started_sqld_url"
            start_sqld migration
            export CUENV_INFRASTRUCTURE_TEST_TURSO_MIGRATION_URL="$started_sqld_url"
          '';
        });

        doc-test-check = craneLib.cargoDocTest (workspaceDocCheckArgs // {
          cargoExtraArgs = workspaceDocCargoExtraArgs;
        });

        bdd-check = craneLib.cargoTest (cuenvCheckArgs // {
          cargoExtraArgs = cuenvPackageCargoExtraArgs;
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
          # Ignore triage (2026-07-02). Each entry is blocked by an upstream
          # dependency chain; drop the group when the named root cause is fixed.
          #
          # rsa (Marvin timing sidechannel, no upstream fix released):
          #   via octocrab -> jsonwebtoken -> rsa 0.9
          #     RUSTSEC-2023-0071
          # wasmtime 41.x (pinned by extism 1.21; patches only in 36.x/42.0.2+/43.0.1+):
          #     RUSTSEC-2026-0085 0086 0087 0088 0089 0091 0092 0093 0094 0095 0096 0114
          # reqwest 0.11 chain (pinned by dagger-sdk 0.20 / graphql_client 0.13;
          # pulls rustls 0.21 -> rustls-webpki 0.101, rustls-pemfile 1.x):
          #     RUSTSEC-2025-0134 (rustls-pemfile unmaintained)
          #     RUSTSEC-2026-0098 0099 0104 (rustls-webpki 0.101)
          buildPhaseCargoCommand = ''
            cargo audit --db ${rustsec-advisory-db} --no-fetch --deny warnings \
              --ignore yanked \
              --ignore RUSTSEC-2023-0071 \
              --ignore RUSTSEC-2025-0134 \
              --ignore RUSTSEC-2026-0085 \
              --ignore RUSTSEC-2026-0086 \
              --ignore RUSTSEC-2026-0087 \
              --ignore RUSTSEC-2026-0088 \
              --ignore RUSTSEC-2026-0089 \
              --ignore RUSTSEC-2026-0091 \
              --ignore RUSTSEC-2026-0092 \
              --ignore RUSTSEC-2026-0093 \
              --ignore RUSTSEC-2026-0094 \
              --ignore RUSTSEC-2026-0095 \
              --ignore RUSTSEC-2026-0096 \
              --ignore RUSTSEC-2026-0098 \
              --ignore RUSTSEC-2026-0099 \
              --ignore RUSTSEC-2026-0104 \
              --ignore RUSTSEC-2026-0114
          '';
          nativeBuildInputs = [ pkgs.cargo-audit ];
        };

        # Main package build
        # On Linux: Use cargo-zigbuild for portable glibc 2.17 binaries
        # On macOS: Use regular cargo with deployment target env var
        cuenv = craneLib.buildPackage (commonArgs // {
          cargoArtifacts = cuenvCargoArtifacts;
          cargoExtraArgs = cuenvPackageCargoExtraArgs;
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
            cargo zigbuild --release ${cuenvPackageCargoExtraArgs} --target ${zigbuildTarget}
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
            cargo build --release ${cuenvPackageCargoExtraArgs}
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
          cuenv-fake-terraform-provider = fake-terraform-provider;
          cuenv-nextest = nextest-check;
        } // pkgs.lib.optionalAttrs pkgs.stdenv.isLinux {
          # Linux only: the suites need sqld, the nixpkgs Terraform providers and
          # loopback networking in the build sandbox, none of which has been
          # validated on darwin.
          cuenv-infrastructure-e2e = infrastructure-e2e-check;
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
