{
  description = "cuenv - Configuration utilities and validation engine";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";
    # Packages newer than the stable release: RustFS and Cuetty's dependencies.
    nixpkgs-unstable.url = "github:NixOS/nixpkgs/nixos-unstable";
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

  outputs = { self, nixpkgs, nixpkgs-unstable, crane, advisory-db, flake-utils, rust-overlay, ... }:
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
        pkgsUnstable = nixpkgs-unstable.legacyPackages.${system};
        cuettyPkgs = import nixpkgs-unstable {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };
        # Cargo build outputs are ignored by Git but can exist in a task's
        # materialized input tree. Exclude them from every flake source.
        sourceFilter = path: _type: builtins.baseNameOf path != "target";
        filteredSource = path: builtins.path {
          inherit path;
          filter = sourceFilter;
        };
        rustfmtOptions = "imports_granularity=Module,hex_literal_case=Lower,format_macro_matchers=true";
        # These command-line options work with the pinned stable rustfmt even
        # though its configuration-file parser rejects them. Verify each toolchain.
        rustfmtProbe = ''
          printf '%s\n' \
            'use std::{path::Path, sync::Arc};' \
            'const COLOR: u32 = 0xABCDEF;' \
            'macro_rules! demo {' \
            '    ( $a: ident , $b: expr ) => { ($a, $b) };' \
            '}' |
            rustfmt --edition 2024 --config ${rustfmtOptions} > "$TMPDIR/style-actual.rs"
          printf '%s\n' \
            'use std::path::Path;' \
            'use std::sync::Arc;' \
            'const COLOR: u32 = 0xabcdef;' \
            'macro_rules! demo {' \
            '    ($a:ident, $b:expr) => {' \
            '        ($a, $b)' \
            '    };' \
            '}' > "$TMPDIR/style-expected.rs"
          diff -u "$TMPDIR/style-expected.rs" "$TMPDIR/style-actual.rs"
        '';
        cuetty = import ./apps/cuetty/nix.nix {
          pkgs = cuettyPkgs;
          inherit crane cue-bridge pinDepsVersions pinLockVersions rustfmtOptions rustfmtProbe sourceFilter;
        };

        # Native packages and checks need the compiler and lint/format tools,
        # not Rust documentation, an analyzer, or four platforms' std libraries.
        rustToolchain = pkgs.rust-bin.stable."1.98.1".minimal.override {
          extensions = [ "clippy" "rustfmt" ];
        };

        rustDevToolchain = pkgs.rust-bin.stable."1.98.1".default.override {
          extensions = [ "rust-src" "rust-analyzer" "clippy" "rustfmt" "llvm-tools-preview" ];
          targets = [
            "x86_64-unknown-linux-gnu"
            "aarch64-unknown-linux-gnu"
            "aarch64-apple-darwin"
            "x86_64-apple-darwin"
          ];
        };

        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;
        craneDevLib = (crane.mkLib pkgs).overrideToolchain rustDevToolchain;

        # Read version from Cargo.toml
        cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
        version = cargoToml.workspace.package.version;

        # Dependency-only derivations must not change with the release number,
        # or every release rebuilds every dependency on every platform. They
        # build a stand-in workspace whose members all carry this version, in
        # the manifests (below) and in the lockfile (`depsCargoLockContents`).
        depsWorkspaceVersion = "0.0.0";

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

        rustsec-advisory-db = pkgs.runCommand "rustsec-advisory-db-sanitized"
          {
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

        # CUE bridge builder. The sources are the Go files, the module files, the
        # checked-in header and the one fixture the Go tests read, so an edit to
        # the crate's Rust code, changelog or benches leaves the bridge alone. The
        # version is a constant for the same reason: a release must not rebuild it.
        cueBridgeSource = builtins.path {
          name = "cue-bridge-source";
          path = fs.toSource {
            root = ./crates/cuengine;
            fileset = fs.intersection targetFreeFileset (fs.unions [
              (fs.fileFilter (file: file.hasExt "go") ./crates/cuengine)
              ./crates/cuengine/go.mod
              ./crates/cuengine/go.sum
              ./crates/cuengine/testdata
              (fs.fileFilter (file: file.hasExt "c" || file.hasExt "h" || file.hasExt "s" || file.hasExt "S") ./crates/cuengine)
            ]);
          };
        };
        cueBridgeArgs = {
          pname = "libcue-bridge";
          version = "0.0.0";
          src = cueBridgeSource;
          vendorHash = "sha256-2127EeuvrvXzjJQNgN3yVd7il9PCISlS+8+N1sUlYPQ=";
          nativeBuildInputs = pkgs.lib.optionals (!pkgs.stdenv.hostPlatform.isDarwin) [ pkgs.binutils ];
        };
        buildGoModule126 = pkgs.buildGoModule.override { go = pkgs.go_1_26; };

        # The archives alone. Every Rust derivation depends on this one, so it
        # runs no tests: they are `cue-bridge-test`, a flake check of its own.
        cue-bridge = buildGoModule126 (cueBridgeArgs // {
          # buildGoModule's default buildPhase would not produce a C archive, and
          # overriding it leaves `getGoDirs` undefined, so the build is explicit
          # and uses the same native CGO toolchain as the tests.
          buildPhase = ''
            runHook preBuild

            export CGO_ENABLED=1

            mkdir -p $out/debug $out/release
            go_sources=$(find . -maxdepth 1 -name '*.go' ! -name '*_test.go' -print | sort)

            go build -buildmode=c-archive -o $out/debug/libcue_bridge.a $go_sources
            cp libcue_bridge.h $out/debug/

            CGO_ENABLED=1 go build -ldflags="-s -w" -buildmode=c-archive -o $out/release/libcue_bridge.a $go_sources
            cp libcue_bridge.h $out/release/

            runHook postBuild
          '';

          doCheck = false;

          installPhase = ''
            runHook preInstall
            runHook postInstall
          '';

          meta = with pkgs.lib; {
            description = "Go CUE bridge library for cuenv";
            license = with licenses; [ mit asl20 ];
            platforms = platforms.unix ++ platforms.windows;
          };
        });

        # `go vet` and the full Go test suite, including the scaling experiment
        # that is otherwise skipped. Nothing depends on this derivation, so Rust
        # builds start without waiting for it.
        cue-bridge-test = cue-bridge.overrideAttrs (_: {
          # Only the derivation's own name changes: the vendored modules stay
          # the bridge's, rather than a second download of the same files.
          name = "libcue-bridge-test-0.0.0";
          inherit (cue-bridge) goModules;
          buildPhase = ''
            runHook preBuild
            runHook postBuild
          '';

          doCheck = true;
          checkPhase = ''
            runHook preCheck

            export CGO_ENABLED=1

            go vet ./...
            # Include the scaling experiment instead of silently skipping it.
            CUENV_FROZEN_SCALING=1 GOMAXPROCS=4 go test -timeout 30m ./...

            runHook postCheck
          '';

          installPhase = ''
            runHook preInstall
            mkdir -p $out
            runHook postInstall
          '';
        });

        # The files every Rust derivation builds from, as an explicit fileset.
        # Membership depends only on nix/source-files.json, never on what else
        # sits in the directory: a git checkout and a `cuenv task` exec root
        # holding the same files evaluate to the same store paths, so one
        # build serves both through the binary cache. env.cue embeds the same
        # list as those tasks' inputs, and `flake-source-check` proves it.
        sourceFiles = builtins.fromJSON (builtins.readFile ./nix/source-files.json);
        fs = pkgs.lib.fileset;
        toRepoPath = relative: ./. + "/${relative}";
        sourceFileset = fs.unions (
          map toRepoPath (sourceFiles.files ++ sourceFiles.trees)
          ++ map (tree: fs.fileFilter (file: file.hasExt "cue") (toRepoPath tree)) sourceFiles.cueTrees
        );
        targetFreeFileset = fs.fromSource (pkgs.lib.cleanSourceWith {
          src = ./.;
          filter = sourceFilter;
        });
        #
        # Crane looks for manifests and lockfiles in a filtered source's
        # original directory rather than the filtered tree
        # (ipetkov/crane#46), which would bring back any Cargo project that
        # happens to sit in the checkout. A plain store path of the fileset
        # leaves it nothing else to find.
        toCraneSource = fileset: builtins.path {
          name = "source";
          path = fs.toSource {
            root = ./.;
            fileset = fs.intersection fileset targetFreeFileset;
          };
        };
        # This standalone workspace has its own lockfile and must not resolve
        # dependencies through the root workspace's Cargo vendor directory.
        functionEnumSource = builtins.path {
          name = "function-enum-source";
          path = fs.toSource {
            root = ./tools/function-enum;
            fileset = fs.intersection targetFreeFileset (fs.unions (
              map toRepoPath (builtins.filter
                (path: pkgs.lib.hasPrefix "tools/function-enum/" path)
                (sourceFiles.fmtFiles ++ sourceFiles.fmtTrees))
            ));
          };
        };
        # Inspect all first-party Rust trees, including applications and tools
        # outside the root Cargo workspace.
        # Filter before walking so local build and dependency trees never enter
        # the inventory used to detect an undeclared top-level Rust source.
        rustScannerInventory = builtins.path {
          name = "rust-scanner-inventory";
          path = ./.;
          filter = path: type:
            let name = builtins.baseNameOf path;
            in if type == "directory"
              then !(builtins.elem name [ ".git" "node_modules" "result" "target" ])
              else pkgs.lib.hasSuffix ".rs" name;
        };
        rustScannerPaths =
          let
            walk = directory:
              let entries = builtins.readDir directory;
              in pkgs.lib.concatMap (name:
                let path = "${directory}/${name}";
                in if entries.${name} == "directory" then walk path else [ path ]
              ) (builtins.attrNames entries);
          in walk rustScannerInventory;
        rustOutsideScannerRoots = builtins.filter (path:
          let relative = pkgs.lib.removePrefix "${rustScannerInventory}/" path;
          in !(pkgs.lib.any (root: pkgs.lib.hasPrefix "${root}/" relative) [ "crates" "apps" "tools" ])
        ) rustScannerPaths;
        rustParameterFiles = fs.intersection targetFreeFileset (fs.unions (
          map (tree: fs.fileFilter (file:
            file.hasExt "rs" || (file.type == "symlink" && file.name != "result")
          ) (toRepoPath tree))
            ([ "crates" ] ++ sourceFiles.rustStyleTrees)
        ));
        rustParameterInputs = fs.unions (
          [ ./crates ] ++ map toRepoPath (
            sourceFiles.cuettyFiles ++ sourceFiles.cuettyTrees
            ++ sourceFiles.fmtFiles ++ sourceFiles.fmtTrees
            ++ sourceFiles.rustStyleFiles ++ sourceFiles.rustStyleInputTrees
          )
        );
        rustParameterUnlisted = fs.toList (fs.difference rustParameterFiles rustParameterInputs);
        rustParameterSource = if rustOutsideScannerRoots != [ ]
          then throw "Rust source lies outside the declared scanner roots: ${
            pkgs.lib.concatMapStringsSep ", " toString rustOutsideScannerRoots
          }"
          else if rustParameterUnlisted != [ ]
          then throw "Rust style sources must be listed in nix/source-files.json: ${
            pkgs.lib.concatMapStringsSep ", " toString rustParameterUnlisted
          }"
          else toCraneSource rustParameterFiles;
        # Dependency builds take it too: crane reduces it to manifests,
        # lockfiles and stub targets, so their artifacts stay cached when
        # source code changes.
        src = toCraneSource sourceFileset;

        # Crane builds dependencies from a dummy source made of the workspace's
        # manifests with their targets stubbed. Those manifests keep the version
        # of every member and every `path` dependency's version requirement, so
        # the dummy source carried the release number. Pin both here. Nothing is
        # compiled from them but empty stand-ins for the workspace's own crates.
        #
        # `shouldPin` says which packages' own versions to pin. The workspace's
        # members all do; a standalone workspace that also carries a crate whose
        # version matters (cuetty's patched stacksafe) names the ones to move.
        pinDepsVersions = { shouldPin ? _name: true }: baseLib: baseLib.overrideScope (_final: prev: {
          cleanCargoToml = args:
            let
              pinPathDependencies = wanted: pkgs.lib.mapAttrs (name: dependency:
                if builtins.isAttrs dependency && dependency ? path && dependency ? version
                  && wanted (dependency.package or name)
                then dependency // { version = depsWorkspaceVersion; }
                else dependency);
              pinSection = wanted: name: toml:
                if toml ? ${name} then { ${name} = pinPathDependencies wanted toml.${name}; } else { };
              pinTablesFor = wanted: toml: toml
                // pinSection wanted "dependencies" toml
                // pinSection wanted "dev-dependencies" toml
                // pinSection wanted "build-dependencies" toml;
              # A member's own tables honour the predicate. The root manifest's
              # `[workspace.dependencies]` lists the release's crates with the
              # release's version, so it always pins.
              pinTables = pinTablesFor shouldPin;
              cleaned = prev.cleanCargoToml args;
              pinnedPackage = if cleaned ? package && !(cleaned.package.version or { } ? workspace)
                  && shouldPin (cleaned.package.name or "")
                then { package = cleaned.package // { version = depsWorkspaceVersion; }; }
                else { };
              pinnedWorkspace = if cleaned ? workspace then {
                workspace = pinTablesFor (_: true) cleaned.workspace
                  // pkgs.lib.optionalAttrs (cleaned.workspace ? package && cleaned.workspace.package ? version) {
                    package = cleaned.workspace.package // { version = depsWorkspaceVersion; };
                  };
              } else { };
              pinnedTargets = if cleaned ? target then {
                target = pkgs.lib.mapAttrs (_: pinTables) cleaned.target;
              } else { };
            in
            pinTables cleaned // pinnedPackage // pinnedWorkspace // pinnedTargets;
        });
        depsCraneLib = pinDepsVersions { } craneLib;

        # The lockfile of that stand-in workspace: the same file with the
        # version of every package that does not come from a registry or git
        # pinned. `cargo --locked` accepts it because those packages are
        # exactly the workspace members the manifests above describe.
        pinLockVersions = { shouldPin ? _name: true }: lockContents:
          let
            blocks = pkgs.lib.splitString "[[package]]\n" lockContents;
            blockName = block:
              let
                nameLines = builtins.filter (pkgs.lib.hasPrefix "name = \"") (pkgs.lib.splitString "\n" block);
              in
              if nameLines == [ ] then ""
              else pkgs.lib.removeSuffix "\"" (pkgs.lib.removePrefix "name = \"" (builtins.head nameLines));
            pinBlock = block:
              if pkgs.lib.hasInfix "\nsource = " block || !(shouldPin (blockName block)) then block
              else pkgs.lib.concatStringsSep "\n" (map
                (line: if pkgs.lib.hasPrefix "version = \"" line
                  then "version = \"${depsWorkspaceVersion}\""
                  else line)
                (pkgs.lib.splitString "\n" block));
          in
          builtins.head blocks
          + pkgs.lib.concatMapStrings (block: "[[package]]\n" + pinBlock block) (builtins.tail blocks);
        depsCargoLockContents = pinLockVersions { } (builtins.readFile ./Cargo.lock);

        # Bridge setup helper
        setupBridge = ''
          mkdir -p target/debug target/release

          rm -f target/debug/libcue_bridge.*
          rm -f target/release/libcue_bridge.*

          cp -r ${cue-bridge}/debug/* target/debug/
          cp -r ${cue-bridge}/release/* target/release/

          chmod -R +w target
        '';

        # The development shell hook runs on every shell entry and every
        # `cuenv exec`, against a target directory that can hold hundreds of
        # thousands of files. Copy the bridge only when the built one changed
        # or a Go source is newer than the copy (the build script would
        # otherwise judge it stale and rebuild it), and make only the copied
        # files writable.
        setupDevBridge = ''
          __bridgeMarker="target/.cue-bridge-source"
          if [ "$(cat "$__bridgeMarker" 2>/dev/null)" != "${cue-bridge}" ] \
            || [ ! -f target/debug/libcue_bridge.a ] \
            || [ ! -f target/release/libcue_bridge.a ] \
            || [ -n "$(find crates/cuengine -maxdepth 1 \
                \( -name '*.go' -o -name go.mod -o -name go.sum \) \
                -newer target/debug/libcue_bridge.a -print -quit)" ]; then
            mkdir -p target/debug target/release
            rm -f target/debug/libcue_bridge.* target/release/libcue_bridge.*
            cp ${cue-bridge}/debug/* target/debug/
            cp ${cue-bridge}/release/* target/release/
            chmod +w target/debug/libcue_bridge.* target/release/libcue_bridge.*
            printf '%s' "${cue-bridge}" > "$__bridgeMarker"
          fi
          unset __bridgeMarker
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

        lockedCargoExtraArgs = "--locked";
        workspaceAllFeaturesCargoExtraArgs = "${lockedCargoExtraArgs} --workspace --all-features";
        cuenvPackageCargoExtraArgs = "${lockedCargoExtraArgs} --package cuenv";
        # The workspace scope keeps Cargo's feature resolution identical to
        # `cuenv-nextest`, so the BDD runner reuses its dependency artifacts.
        # The CLI enables every one of its features by default.
        bddCargoExtraArgs = "${workspaceAllFeaturesCargoExtraArgs} --test bdd";

        # Test checks need no debugger payload. Keep dependency and consumer
        # profiles identical so Cargo can reuse every compiled dependency.
        ciBuildArgs = {
          CARGO_PROFILE = "ci";
          CARGO_PROFILE_CI_DEBUG = "0";
        };

        # Keep dependency-only derivations aligned with downstream cargo flags.
        # Mixing workspace-wide and package-specific scopes forces Cargo to rebuild.
        #
        # They also depend on nothing that changes with a release or with the
        # bridge. Crane replaces every build script with an empty one in the
        # dummy source (`package.build` points at it), so the Go bridge, the
        # 1Password SDK and the tools only the bridge's and the CLI's own build
        # scripts use are not needed to compile dependencies.
        # The source leaves out Cargo.lock: Crane copies the file it finds in the
        # source into the dummy one, so the release number would leak back in.
        depsOnlySource = toCraneSource (fs.difference sourceFileset ./Cargo.lock);
        cargoArtifactsArgs = {
          src = depsOnlySource;
          version = depsWorkspaceVersion;
          cargoLock = builtins.toFile "Cargo.lock" depsCargoLockContents;
          preBuild = "";
          buildInputs = platformBuildInputs;
          nativeBuildInputs = [ pkgs.pkg-config ];
        };
        depsOnlyCommonArgs = builtins.removeAttrs commonArgs [
          "CUE_BRIDGE_PATH"
          "ONEPASSWORD_WASM_PATH"
        ];
        buildDepsOnly = depsCraneLib.buildDepsOnly;

        workspaceCargoArtifacts = buildDepsOnly (depsOnlyCommonArgs // cargoArtifactsArgs // {
          cargoExtraArgs = workspaceAllFeaturesCargoExtraArgs;
          # Clippy consumes metadata, so building and linking release binaries
          # and test executables here only wastes compilation and cache space.
          doCheck = false;
          buildPhaseCargoCommand = "cargoWithProfile check ${workspaceAllFeaturesCargoExtraArgs} --all-targets";
        });

        # Test binaries are built with the lightweight `ci` profile (no LTO,
        # default codegen-units) so the workspace-wide nextest compile stays
        # within CI runner memory. The release binary keeps `[profile.release]`
        # (thin LTO).
        workspaceCiCargoArtifacts = buildDepsOnly (depsOnlyCommonArgs // cargoArtifactsArgs // ciBuildArgs // {
          cargoExtraArgs = workspaceAllFeaturesCargoExtraArgs;
          # One pass prepares regular and dev dependencies together. Crane's
          # default check/build/test sequence compiles them three times.
          doCheck = false;
          buildPhaseCargoCommand = "cargoWithProfile test ${workspaceAllFeaturesCargoExtraArgs} --no-run";
        });

        cuenvCargoArtifacts = buildDepsOnly (depsOnlyCommonArgs // cargoArtifactsArgs // {
          cargoExtraArgs = cuenvPackageCargoExtraArgs;
          # The package is smoke-tested after installation; its Rust tests have
          # dedicated checks and do not belong in a bootstrap dependency build.
          doCheck = false;
          buildPhaseCargoCommand = "cargoWithProfile build ${cuenvPackageCargoExtraArgs}";
        });

        workspaceCheckArgs = commonArgs // {
          cargoArtifacts = workspaceCargoArtifacts;
          doCheck = true;
          # Terminal checks have no artifact consumers. Only dependency builds
          # and packages need their Cargo target directories in the cache.
          doInstallCargoArtifacts = false;
        };

        cuenvCheckArgs = commonArgs // ciBuildArgs // {
          cargoArtifacts = workspaceCiCargoArtifacts;
          doCheck = true;
          doInstallCargoArtifacts = false;
        };

        styleClippyArgs = "--all-targets -- -D warnings -D clippy::wildcard_imports -D clippy::undocumented_unsafe_blocks -D clippy::needless_for_each";
        workspace-clippy-check = craneLib.cargoClippy (workspaceCheckArgs // {
          cargoExtraArgs = workspaceAllFeaturesCargoExtraArgs;
          cargoClippyExtraArgs = styleClippyArgs;
        });
        function-enum-clippy-check = craneLib.cargoClippy {
          src = functionEnumSource;
          pname = "cuenv-function-enum";
          version = (builtins.fromTOML (builtins.readFile ./tools/function-enum/Cargo.toml)).package.version;
          cargoArtifacts = null;
          cargoExtraArgs = "--locked --all-features";
          cargoClippyExtraArgs = styleClippyArgs;
          doInstallCargoArtifacts = false;
        };
        function-parameter-check = craneLib.buildPackage {
          src = functionEnumSource;
          pname = "cuenv-rust-source-style";
          version = (builtins.fromTOML (builtins.readFile ./tools/function-enum/Cargo.toml)).package.version;
          cargoArtifacts = null;
          cargoExtraArgs = "--locked";
          doCheck = true;
          doInstallCargoArtifacts = false;
          postCheck = ''
            cargo run --release --locked -- --check-style --path ${rustParameterSource}
          '';
        };
        clippy-check = pkgs.linkFarm "cuenv-clippy" [
          { name = "workspace"; path = workspace-clippy-check; }
          { name = "function-enum"; path = function-enum-clippy-check; }
        ];

        nextest-check = craneLib.cargoNextest (workspaceCheckArgs // ciBuildArgs // {
          cargoArtifacts = workspaceCiCargoArtifacts;
          cargoExtraArgs = workspaceAllFeaturesCargoExtraArgs;
          nativeBuildInputs = commonArgs.nativeBuildInputs ++ [ pkgs.fish pkgs.zsh pkgs.powershell ];
          CUENV_TEST_PWSH = "${pkgs.powershell}/bin/pwsh";
          # Hook supervisor tests must exercise the CLI instead of passing when
          # an independently built binary is unavailable. They take the one
          # this check builds for the CLI's own integration tests: depending
          # on the release package put the release profile's LTO build of the
          # whole workspace in front of every test run.
          preCheck = ''
            export CUENV_EXECUTABLE="$PWD/''${CARGO_TARGET_DIR:-target}/ci/cuenv"
          '';
          # HTTP client construction loads native roots, including in mock tests.
          SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
        });

        # Fake Terraform provider used by the infrastructure end-to-end suites. It
        # reproduces provider behaviours real providers show only occasionally;
        # `go vet` runs as its check phase because the source is not part of the
        # cuengine Go module.
        fake-terraform-provider = pkgs.buildGoModule {
          pname = "terraform-provider-fake";
          version = "0.0.0";
          src = filteredSource ./crates/infrastructure/tests/fake_provider;
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

        # Real Terraform protocol codecs used by the exact-number round-trip
        # test; dependencies are vendored so the E2E check stays offline.
        numeric-interop = (pkgs.buildGoModule.override { go = pkgs.go_1_26; }) {
          pname = "cuenv-numeric-interop";
          version = "0.0.0";
          src = filteredSource ./crates/infrastructure/tests/numeric_interop;
          vendorHash = "sha256-0tNx+rVKuNpAOzCtHd9Gpf/g+gtvzpmq4UShMsOKAS8=";
          doCheck = true;
          checkPhase = ''
            runHook preCheck
            go vet ./...
            runHook postCheck
          '';
        };

        # The pinned official Cloudflare binary is exercised only against a
        # loopback fake API; the test is currently x86_64-linux only.
        cloudflare-test-provider = import ./crates/infrastructure/tests/cloudflare_provider.nix { inherit pkgs; };

        # The ignored suites need a libSQL server and real provider binaries, so
        # they are skipped by `cuenv-nextest`. This check provides them inside the
        # Nix sandbox (loopback only, no network): sqld and a RustFS S3 server on
        # free loopback ports, the fake provider and the hashicorp random, local and tfe providers from
        # nixpkgs. `installs_provider_from_registry` is excluded because it
        # downloads from registry.terraform.io. Only ignored tests run here; the
        # rest of each crate's tests belong to `cuenv-nextest`.
        #
        # Only the crates and targets the suites live in are built, without debug
        # information, with their own dependency artifacts: the workspace-wide
        # artifacts of `cuenv-nextest` would compile every test binary again.
        infrastructureE2eTestFiles = [
          "crates/infrastructure/tests/provider_end_to_end.rs"
          "crates/cuenv/tests/infrastructure_lifecycle.rs"
        ] ++ pkgs.lib.optionals (system == "x86_64-linux") [
          "crates/infrastructure/tests/cloudflare_offline.rs"
        ];
        infrastructureE2eCargoExtraArgs = builtins.concatStringsSep " " ([
          lockedCargoExtraArgs
          "--package cuenv-infrastructure"
          "--package cuenv"
          "--lib"
        ] ++ map
          (testFile: "--test ${pkgs.lib.removeSuffix ".rs" (builtins.baseNameOf testFile)}")
          infrastructureE2eTestFiles);

        # Apply the Rust style guide's import, hex, and macro formatting rules.
        fmt-check = craneLib.cargoFmt {
          src = toCraneSource (
            fs.unions ([ sourceFileset ] ++ map toRepoPath (sourceFiles.fmtFiles ++ sourceFiles.fmtTrees))
          );
          pname = "cuenv";
          inherit version;
          rustFmtExtraArgs = "--config ${rustfmtOptions}";
          preBuild =
            assert pkgs.lib.assertMsg
              ((builtins.fromTOML (builtins.readFile ./treefmt.toml)).formatter.rustfmt.options == [
                "--edition"
                "2024"
                "--config"
                rustfmtOptions
              ])
              "treefmt Rust options must match the Nix formatting checks";
            rustfmtProbe;
          postBuild = ''
            cargo fmt --manifest-path tools/function-enum/Cargo.toml --all -- --check --config ${rustfmtOptions}
          '';
        };

        infrastructureE2eCargoArtifacts = buildDepsOnly (depsOnlyCommonArgs // cargoArtifactsArgs // ciBuildArgs // {
          pname = "cuenv-infrastructure-e2e";
          cargoExtraArgs = infrastructureE2eCargoExtraArgs;
          doCheck = false;
          buildPhaseCargoCommand = "cargoWithProfile test ${infrastructureE2eCargoExtraArgs} --no-run";
          # These targets are discovered from tests/*.rs rather than declared
          # in Cargo.toml, so Crane's dummy source needs explicit placeholders.
          extraDummyScript = pkgs.lib.concatMapStringsSep "\n" (testFile: ''
            mkdir -p "$out/${builtins.dirOf testFile}"
            printf '%s\n' '//! Dependency-only integration test target.' > "$out/${testFile}"
          '') infrastructureE2eTestFiles;
        });

        infrastructure-e2e-check = craneLib.cargoNextest (workspaceCheckArgs // ciBuildArgs // {
          pname = "cuenv-infrastructure-e2e";
          cargoArtifacts = infrastructureE2eCargoArtifacts;
          cargoExtraArgs = infrastructureE2eCargoExtraArgs;
          nativeBuildInputs = commonArgs.nativeBuildInputs ++ [
            pkgs.curl
            pkgsUnstable.rustfs
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
            # Tests that need the network are left out: a registry download, and
            # a real Cloudflare D1 database (the D1 backend runs here against a
            # SQLite-backed stand-in in the default suite instead).
            "'(package(cuenv-infrastructure) | (package(cuenv) & binary(infrastructure_lifecycle))) & not test(installs_provider_from_registry) & not test(lifecycle_against_d1)'"
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
            # Each test starts its own libSQL server from this.
            export CUENV_INFRASTRUCTURE_TEST_SQLD="${pkgs.sqld}/bin/sqld"
            export CUENV_INFRASTRUCTURE_TEST_FAKE_PROVIDER="${fake-terraform-provider}/bin/terraform-provider-fake"
            export CUENV_INFRASTRUCTURE_TEST_NUMERIC_INTEROP="${numeric-interop}/bin/numeric-interop"
            ${pkgs.lib.optionalString (system == "x86_64-linux") ''
              export CUENV_INFRASTRUCTURE_TEST_CLOUDFLARE_PROVIDER="${cloudflare-test-provider}/bin/terraform-provider-cloudflare_v5.28.1"
            ''}
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

            server_pids=""
            stop_servers() {
              for pid in $server_pids; do
                kill "$pid" 2>/dev/null || true
              done
            }
            # Registered as a failure hook and called again in postCheck rather than
            # trapped on EXIT: a trap would replace stdenv's own exit handler.
            failureHooks+=(stop_servers)

            # A real S3-compatible server for the object-storage conformance suite:
            # conditional writes, ETags, listings and SigV4 against something other
            # than cuenv's own fake.
            s3_port="$(free_port)"
            export CUENV_TEST_S3_ENDPOINT="http://127.0.0.1:$s3_port"
            export CUENV_TEST_S3_BUCKET="cuenv-state"
            export CUENV_TEST_S3_ACCESS_KEY_ID="cuenv-e2e"
            export CUENV_TEST_S3_SECRET_ACCESS_KEY="cuenv-e2e-secret"
            mkdir -p "$TMPDIR/rustfs"
            RUSTFS_ACCESS_KEY="$CUENV_TEST_S3_ACCESS_KEY_ID" \
              RUSTFS_SECRET_KEY="$CUENV_TEST_S3_SECRET_ACCESS_KEY" \
              rustfs server --address "127.0.0.1:$s3_port" "$TMPDIR/rustfs" \
              > "$TMPDIR/rustfs.log" 2>&1 &
            server_pids="$server_pids $!"
            # RustFS answers its health endpoint before its storage is ready (503
            # on S3 requests), so readiness is the bucket's creation itself.
            bucket_created=""
            for _ in $(seq 1 150); do
              if curl --silent --fail --output /dev/null \
                --aws-sigv4 "aws:amz:us-east-1:s3" \
                --user "$CUENV_TEST_S3_ACCESS_KEY_ID:$CUENV_TEST_S3_SECRET_ACCESS_KEY" \
                --request PUT "$CUENV_TEST_S3_ENDPOINT/$CUENV_TEST_S3_BUCKET"; then
                bucket_created=1
                break
              fi
              sleep 0.2
            done
            if [ -z "$bucket_created" ]; then
              echo "rustfs did not accept the bucket" >&2
              cat "$TMPDIR/rustfs.log" >&2
              exit 1
            fi
          '';
          postCheck = ''
            stop_servers
          '';
          # The check's outcome is all that matters; installing its target
          # directory only adds a large artifact to the store and the cache.
          doInstallCargoArtifacts = false;
        });

        # Documentation tests share the test profile and dependency artifacts
        # of `cuenv-nextest`. A dependency build of their own, in Crane's
        # default release profile, compiled every dependency once more.
        doc-test-check = craneLib.cargoDocTest (workspaceCheckArgs // ciBuildArgs // {
          cargoArtifacts = workspaceCiCargoArtifacts;
          cargoExtraArgs = workspaceAllFeaturesCargoExtraArgs;
        });

        bdd-check = craneLib.cargoTest (cuenvCheckArgs // {
          cargoExtraArgs = bddCargoExtraArgs;
          # Exercise the real runner's exit status as well as its happy path.
          # A failing scenario must make cargo and the Nix check fail.
          postCheck = ''
            probe="crates/cuenv/tests/bdd/features/ci-exit-status.feature"
            cat > "$probe" <<'EOF'
            Feature: CI exit status
              Scenario: CI rejects a failing scenario
                Given cuenv is installed and available
                When I run "cuenv --version"
                Then the exit code should be 42
            EOF

            probe_status=0
            cargoWithProfile test ${bddCargoExtraArgs} -- --name 'CI rejects a failing scenario' > "$TMPDIR/bdd-exit-status.log" 2>&1 || probe_status=$?
            rm "$probe"
            if [ "$probe_status" -eq 0 ] || ! grep -q '1 failed' "$TMPDIR/bdd-exit-status.log"; then
              cat "$TMPDIR/bdd-exit-status.log" >&2
              echo "BDD failure did not produce the expected nonzero exit" >&2
              exit 1
            fi
            echo "Verified BDD exits nonzero for a failing scenario."
          '';
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
          buildPhaseCargoCommand = ''
            # Yanked releases were already ignored; avoid querying a registry
            # index that is unavailable in this offline derivation.
            if [ -e .cargo/audit.toml ]; then
              echo "Update the audit derivation to account for .cargo/audit.toml" >&2
              exit 1
            fi
            mkdir -p .cargo
            printf '[yanked]\nenabled = false\n' > .cargo/audit.toml

            cargo audit --db ${rustsec-advisory-db} --no-fetch --deny warnings \
              --ignore RUSTSEC-2023-0071 \
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
              --ignore RUSTSEC-2026-0114

            # Verify that disabling the index check does not disable advisories.
            cat > "$TMPDIR/vulnerable.lock" <<'EOF'
            version = 4

            [[package]]
            name = "bytes"
            version = "1.10.1"
            source = "registry+https://github.com/rust-lang/crates.io-index"
            EOF
            if cargo audit --db ${rustsec-advisory-db} --no-fetch --deny warnings \
              --file "$TMPDIR/vulnerable.lock" > "$TMPDIR/audit-probe.log" 2>&1; then
              echo "The audit failed to reject a known vulnerable dependency" >&2
              exit 1
            fi
            if ! grep -q 'RUSTSEC-2026-0007' "$TMPDIR/audit-probe.log"; then
              cat "$TMPDIR/audit-probe.log" >&2
              echo "The audit did not report the expected advisory" >&2
              exit 1
            fi
          '';
          nativeBuildInputs = [ pkgs.cargo-audit ];
        };

        # Nix consumers need the native toolchain and runtime closure on every platform.
        cuenv = craneLib.buildPackage (commonArgs // {
          cargoArtifacts = cuenvCargoArtifacts;
          cargoExtraArgs = cuenvPackageCargoExtraArgs;
          pname = "cuenv";
          inherit version;
          doCheck = false; # Rust tests have dedicated checks below.
          doInstallCheck = true;
          installCheckPhase = ''
            runHook preInstallCheck
            "$out/bin/cuenv" --version
            "$out/bin/cuenv" env print --path examples/env-basic --package examples --output json > "$TMPDIR/cuenv-example-env.json"
            grep -q 'DATABASE_URL' "$TMPDIR/cuenv-example-env.json"
            runHook postInstallCheck
          '';
          meta.mainProgram = "cuenv";
        });

        # Development tools configuration
        devTools = with pkgs; [
          go_1_26
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
        ] ++ lib.optionals stdenv.hostPlatform.isLinux [
          cargo-llvm-cov
          gcc # Provides cc linker for cargo
          patchelf
          libgccjit
          mold # Fast linker for faster link times
          clang # Required for mold integration
        ];

      in
      {
        checks = {
          inherit cuenv;
          inherit (cuetty) cuetty-test cuetty-clippy cuetty-fmt;
          cuenv-audit = audit-check;
          cuenv-bdd = bdd-check;
          cuenv-cue-bridge-test = cue-bridge-test;
          cuenv-clippy = clippy-check;
          cuenv-deny = deny-check;
          cuenv-doctest = doc-test-check;
          cuenv-fake-terraform-provider = fake-terraform-provider;
          cuenv-fmt = fmt-check;
          cuenv-nextest = nextest-check;
          cuenv-rust-parameter-style = function-parameter-check;
        } // pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
          # Building the package also checks its executable and dlopen paths.
          cuetty = cuetty.package;
          # Linux only: the suites need sqld, the nixpkgs Terraform providers and
          # loopback networking in the build sandbox, none of which has been
          # validated on darwin.
          cuenv-infrastructure-e2e = infrastructure-e2e-check;
          cuenv-numeric-interop = numeric-interop;
        };

        packages = {
          default = cuenv;
          inherit cuenv cue-bridge;
          cuetty = cuetty.package;
        };

        devShells.cuetty = cuetty.devShell;

        devShells.default = craneDevLib.devShell ({
          packages = devTools;

          RUST_BACKTRACE = "1";
          RUST_SRC_PATH = "${rustDevToolchain}/lib/rustlib/src/rust/library";
          CUE_BRIDGE_PATH = "${cue-bridge}";

          shellHook = ''
            ${setupDevBridge}

            # sccache configuration — only set if not already provided (e.g. by CI)
            export RUSTC_WRAPPER="''${RUSTC_WRAPPER:-${pkgs.sccache}/bin/sccache}"
            export SCCACHE_DIR="''${SCCACHE_DIR:-$HOME/.cache/sccache}"
            # sccache's 10 GiB default evicts continuously for this workspace.
            export SCCACHE_CACHE_SIZE="''${SCCACHE_CACHE_SIZE:-40G}"

            # Install docs dependencies
            cd docs
            bun install
            
            ${pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
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
            echo "Docs dependencies installed${pkgs.lib.optionalString pkgs.stdenv.hostPlatform.isLinux ", wrangler patched, mold linker available"}"
          '';
        } // pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
          LD_LIBRARY_PATH = "${pkgs.libgccjit}/lib:$LD_LIBRARY_PATH";
        });

        apps = {
          cuetty = (flake-utils.lib.mkApp { drv = cuetty.package; }) // {
            meta.description = "Cuetty terminal";
          };
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
