{
  pkgs,
  crane,
  cue-bridge,
  pinDepsVersions,
  pinLockVersions,
  rustfmtOptions,
  rustfmtProbe,
  sourceFilter,
}:
let
  lib = pkgs.lib;
  rustToolchain = pkgs.rust-bin.stable."1.98.1".minimal.override {
    extensions = [
      "clippy"
      "rustfmt"
    ];
  };
  craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;
  cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
  version = cargoToml.package.version;
  sourceFiles = builtins.fromJSON (builtins.readFile ../../nix/source-files.json);
  fs = lib.fileset;
  toRepoPath = relative: ../.. + "/${relative}";
  # Only these files can change what cuetty's derivations build or test.
  # Every one is listed in nix/source-files.json, so a task exec root holds
  # them all, but the lists there also serve the main workspace and are far
  # wider than this: an edit to another crate must not rebuild the application.
  #
  # - The app: its manifest, lockfile, sources, assets, tests and compat crate.
  #   nix.nix is listed for task exec roots; the Rust derivations do not read it.
  appFiles = map toRepoPath (
    lib.subtractLists [ "apps/cuetty/nix.nix" ] sourceFiles.cuettyFiles ++ sourceFiles.cuettyTrees
  );
  # - cuengine, its only path dependency: manifest, build script, Rust sources
  #   and the Go files the build script fingerprints (the bridge archive itself
  #   comes from the cue-bridge derivation). The root manifest supplies the
  #   workspace keys it inherits, and clippy.toml is found by walking up from
  #   the app's directory.
  cuengineDirectory = toRepoPath "crates/cuengine";
  cuengineFiles =
    let
      isBuildInput =
        name:
        name == "Cargo.toml"
        || name == "build.rs"
        || name == "go.mod"
        || name == "go.sum"
        || (lib.hasSuffix ".go" name && !lib.hasSuffix "_test.go" name);
    in
    [
      (toRepoPath "crates/cuengine/src")
      # Cargo refuses to load the manifest unless the declared benchmark exists.
      (toRepoPath "crates/cuengine/benches/evaluation_benchmarks.rs")
    ]
    ++ lib.mapAttrsToList (name: _: cuengineDirectory + "/${name}") (
      lib.filterAttrs (name: type: type == "regular" && isBuildInput name) (
        builtins.readDir cuengineDirectory
      )
    );
  workspaceFiles = map toRepoPath [
    "Cargo.toml"
    "clippy.toml"
  ];
  # - The repository's CUE package, which `discovers_the_repository_cuenv_task_catalog`
  #   evaluates: env.cue (and the source-files.json it embeds), the module
  #   file, the schema, and the three contrib packages env.cue imports.
  repositoryCueFiles = [
    (toRepoPath "env.cue")
    (toRepoPath "nix/source-files.json")
    (toRepoPath "cue.mod/module.cue")
  ]
  ++ map (tree: fs.fileFilter (file: file.hasExt "cue") (toRepoPath tree)) [
    "schema"
    "contrib/codecov"
    "contrib/contributors"
    "contrib/nix"
  ];
  sourceOf =
    fileset:
    builtins.path {
      # Crane looks for manifests in a filtered source's original directory
      # (ipetkov/crane#46), so give it a plain store path of the fileset.
      name = "source";
      path = fs.toSource {
        root = ../..;
        fileset = fs.intersection (fs.unions fileset) (
          fs.fromSource (
            lib.cleanSourceWith {
              src = ../..;
              filter = sourceFilter;
            }
          )
        );
      };
    };
  src = sourceOf (appFiles ++ cuengineFiles ++ workspaceFiles ++ repositoryCueFiles);
  # rustfmt follows path dependencies into cuengine's workspace, so it needs
  # every root member's manifest and checks all of their Rust.
  fmtSrc = sourceOf (
    appFiles
    ++ workspaceFiles
    ++ [
      (fs.fileFilter (file: file.hasExt "rs" || file.name == "Cargo.toml") (toRepoPath "crates"))
    ]
  );
  xcodeXcrun = pkgs.writeShellScriptBin "xcrun" ''
    unset DEVELOPER_DIR SDKROOT
    exec /usr/bin/xcrun "$@"
  '';
  nativeBuildInputs = lib.optionals pkgs.stdenv.hostPlatform.isDarwin [ xcodeXcrun ] ++ [
    pkgs.pkg-config
  ];
  buildInputs =
    lib.optionals pkgs.stdenv.hostPlatform.isDarwin [ pkgs.libiconv ]
    ++ lib.optionals pkgs.stdenv.hostPlatform.isLinux [
      pkgs.alsa-lib
      pkgs.fontconfig
      pkgs.freetype
      pkgs.libxkbcommon
      pkgs.openssl
      pkgs.vulkan-loader
      pkgs.wayland
      pkgs.libxcb
      pkgs.libxcursor
      pkgs.libxi
      pkgs.libxrandr
    ];
  # Rio is fetched as a fixed-output derivation, so it is a binary-cache
  # substitute rather than a clone made while evaluating the flake. The key is
  # the lockfile's `source` string for it; the hash covers the checkout the
  # same way crane fetches it (submodules and LFS included).
  rioRevision = cargoToml.dependencies.rio-vt.rev;
  gitOutputHashes = {
    "git+${cargoToml.dependencies.rio-vt.git}?rev=${rioRevision}#${rioRevision}" =
      "sha256-4Ll4RLgqptvPbrsUoSv9TiDPdFL+Tum8P/yf/h+5k6A=";
  };
  # Crane only warns about a git source without a hash and then clones it while
  # evaluating the flake, so fail instead.
  lockedGitSources = lib.unique (
    map (package: package.source) (
      builtins.filter (package: lib.hasPrefix "git+" (package.source or "")) (
        (builtins.fromTOML (builtins.readFile ./Cargo.lock)).package
      )
    )
  );
  missingGitHashes = builtins.filter (source: !(gitOutputHashes ? ${source})) lockedGitSources;
  vendorDirectory =
    assert lib.assertMsg (missingGitHashes == [ ])
      "apps/cuetty/Cargo.lock has git sources without an outputHashes entry in apps/cuetty/nix.nix (new or re-pinned git dependency; add its source string to gitOutputHashes with lib.fakeHash, build, and copy the hash from the mismatch error): ${lib.concatStringsSep ", " missingGitHashes}";
    craneLib.vendorCargoDeps {
      cargoLock = ./Cargo.lock;
      outputHashes = gitOutputHashes;
    };
  # Every derivation runs cargo from the repository root against the app's
  # manifest, so give them all one target directory there: crane unpacks the
  # dependency artifacts into it, and cargo would otherwise look for them in
  # apps/cuetty/target.
  cargoManifestArgs = "--locked --manifest-path apps/cuetty/Cargo.toml";
  commonArgs = {
    pname = "cuetty";
    inherit
      version
      src
      nativeBuildInputs
      buildInputs
      ;
    cargoVendorDir = vendorDirectory;
    CARGO_TARGET_DIR = "target";
    doCheck = false;
    CUE_BRIDGE_PATH = cue-bridge;
  };

  # Third-party crates are compiled once per cargo profile, from a stand-in
  # workspace: the manifests and lockfile with the workspace version pinned and
  # every target stubbed. Neither the sources, the release number nor the Go
  # bridge (their build scripts are stubs) reach these derivations, so a change
  # to cuetty's or cuengine's code recompiles only those crates. Only
  # cuengine's version moves with releases: cuetty's own and the stacksafe
  # adapter's stay, because the adapter must keep satisfying GPUI's `^0.1`.
  pinned = {
    shouldPin = name: name == "cuengine";
  };
  depsLib = pinDepsVersions pinned craneLib;
  stacksafeSource = fs.toSource {
    root = toRepoPath "apps/cuetty/compat/stacksafe";
    fileset = fs.unions [
      (toRepoPath "apps/cuetty/compat/stacksafe/Cargo.toml")
      (toRepoPath "apps/cuetty/compat/stacksafe/src")
    ];
  };
  depsCargoLock = builtins.toFile "Cargo.lock" (
    pinLockVersions pinned (builtins.readFile ./Cargo.lock)
  );
  depsArgs =
    builtins.removeAttrs commonArgs [ "CUE_BRIDGE_PATH" ]
    // {
      cargoLock = depsCargoLock;
      # The lockfile belongs next to the manifest it describes. The stacksafe
      # adapter is a real dependency of GPUI, not one of cuetty's own crates,
      # so it goes in whole: a stub would not export what GPUI imports, and a
      # dummy build script would make cargo see it as changed in the consumers.
      extraDummyScript = ''
        cp ${depsCargoLock} $out/apps/cuetty/Cargo.lock
        rm -rf $out/apps/cuetty/compat/stacksafe
        cp -r ${stacksafeSource} $out/apps/cuetty/compat/stacksafe
        chmod -R u+w $out/apps/cuetty/compat/stacksafe
      '';
    };
  # Test and clippy builds use cargo's dev profile; `CARGO_PROFILE` is empty so
  # crane adds no profile flag.
  devProfile = {
    CARGO_PROFILE = "";
  };
  testFlags = "${cargoManifestArgs} --workspace --all-features --all-targets";
  docFlags = "${cargoManifestArgs} --workspace --all-features --doc";
  clippyLints = "-D warnings -D clippy::wildcard_imports -D clippy::undocumented_unsafe_blocks -D clippy::needless_for_each";
  # nixpkgs' buildRustPackage wraps cargo as `cargo auditable` and passes
  # `--target`, so the binary embeds its dependency list (`.dep-v0`) and cargo
  # puts output under target/<triple>/release. The dependency derivation and
  # the package must run the identical command, or cargo rebuilds everything.
  releaseCommand = "cargo auditable build --release --target ${pkgs.stdenv.hostPlatform.rust.rustcTargetSpec} ${cargoManifestArgs} --bin cuetty";
  releaseNativeBuildInputs = nativeBuildInputs ++ [ pkgs.cargo-auditable ];
  releaseArtifacts = depsLib.buildDepsOnly (
    depsArgs
    // {
      nativeBuildInputs = releaseNativeBuildInputs;
      cargoExtraArgs = "${cargoManifestArgs} --bin cuetty";
      buildPhaseCargoCommand = releaseCommand;
    }
  );
  testArtifacts = depsLib.buildDepsOnly (
    depsArgs
    // devProfile
    // {
      pname = "cuetty-test";
      cargoExtraArgs = testFlags;
      # The doctest pass compiles the adapter crate's dependencies again under
      # its own flags, so it belongs in the preparation too.
      buildPhaseCargoCommand = ''
        cargo test ${testFlags} --no-run
        cargo test ${docFlags}
      '';
    }
  );
  clippyArtifacts = depsLib.buildDepsOnly (
    depsArgs
    // devProfile
    // {
      pname = "cuetty-clippy";
      cargoExtraArgs = testFlags;
      # Cargo runs Clippy's driver for every workspace member, and the stacksafe
      # adapter is one that GPUI depends on, so a plain `cargo check` would
      # leave it, GPUI and everything between them to be checked again. Run the
      # consumer's own command so their compiler invocations match.
      buildPhaseCargoCommand = "cargo clippy ${testFlags} -- ${clippyLints}";
    }
  );
  package = craneLib.buildPackage (
    commonArgs
    // {
      cargoArtifacts = releaseArtifacts;
      buildPhaseCargoCommand = releaseCommand;
      # Crane's default install reads the build log with `cargo metadata` from
      # the repository root, whose workspace lists crates this source omits.
      doNotPostBuildInstallCargoBinaries = true;
      installPhaseCommand = ''
        install -Dm755 "$CARGO_TARGET_DIR/${pkgs.stdenv.hostPlatform.rust.rustcTargetSpec}/release/cuetty" "$out/bin/cuetty"
      '';
      nativeBuildInputs =
        releaseNativeBuildInputs
        ++ lib.optionals pkgs.stdenv.hostPlatform.isLinux [
          pkgs.patchelf
          pkgs.copyDesktopItems
        ];
      desktopItems = lib.optionals pkgs.stdenv.hostPlatform.isLinux [
        (pkgs.makeDesktopItem {
          name = "cuetty";
          desktopName = "Cuetty";
          genericName = "Terminal";
          comment = "Cuenv's GPUI terminal backed by pinned Rio";
          exec = "cuetty";
          icon = "cuetty";
          terminal = false;
          startupWMClass = "cuetty";
          categories = [
            "System"
            "TerminalEmulator"
          ];
          keywords = [
            "shell"
            "terminal"
            "cuenv"
          ];
        })
      ];
      postInstall = lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
        install -Dm644 ${./assets/app/cuetty.svg} "$out/share/icons/hicolor/scalable/apps/cuetty.svg"
      '';
      postFixup = lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
        # GPUI loads some libraries with dlopen, so keep their paths after
        # Nix's RPATH shrinking without exporting them to terminal children.
        patchelf --add-rpath ${lib.escapeShellArg (lib.makeLibraryPath buildInputs)} "$out/bin/cuetty"
      '';
      doInstallCheck = pkgs.stdenv.hostPlatform.isLinux;
      installCheckPhase = lib.optionalString pkgs.stdenv.hostPlatform.isLinux ''
        runHook preInstallCheck

        # The installed entry point must be an ELF executable, not a wrapper
        # that modifies the environment inherited by the user's shell.
        patchelf --print-interpreter "$out/bin/cuetty" > /dev/null

        # The launcher entry names its icon, which has to be where desktops look for it.
        grep -qx 'Icon=cuetty' "$out/share/applications/cuetty.desktop"
        test -s "$out/share/icons/hicolor/scalable/apps/cuetty.svg"

        # Exercise dynamically loaded libraries using the installed ELF's
        # search path, without relying on the build environment's library path.
        cat > "$TMPDIR/cuetty-dlopen-check.c" <<'EOF'
        #include <dlfcn.h>
        #include <stdio.h>

        int main(void) {
            const char *libraries[] = {
                "libwayland-client.so.0",
                "libvulkan.so.1",
                "libfontconfig.so.1",
            };
            for (unsigned int i = 0; i < sizeof(libraries) / sizeof(libraries[0]); i++) {
                void *handle = dlopen(libraries[i], RTLD_NOW);
                if (handle == NULL) {
                    fprintf(stderr, "%s: %s\n", libraries[i], dlerror());
                    return 1;
                }
                dlclose(handle);
            }
            return 0;
        }
        EOF
        cc "$TMPDIR/cuetty-dlopen-check.c" -o "$TMPDIR/cuetty-dlopen-check" -ldl
        patchelf --set-rpath "$(patchelf --print-rpath "$out/bin/cuetty")" "$TMPDIR/cuetty-dlopen-check"
        env -u LD_LIBRARY_PATH "$TMPDIR/cuetty-dlopen-check"

        runHook postInstallCheck
      '';
      meta = {
        description = "Cuenv's GPUI terminal backed by pinned Rio";
        license = lib.licenses.agpl3Plus;
        mainProgram = "cuetty";
      };
    }
  );
  cuetty-test = craneLib.mkCargoDerivation (
    commonArgs
    // devProfile
    // {
      pname = "cuetty-test";
      cargoArtifacts = testArtifacts;
      doInstallCargoArtifacts = false;
      buildPhaseCargoCommand = ''
        cargo test ${testFlags} -- --include-ignored
        cargo test ${docFlags}
      '';
    }
  );
  cuetty-clippy = craneLib.mkCargoDerivation (
    commonArgs
    // devProfile
    // {
      pname = "cuetty-clippy";
      cargoArtifacts = clippyArtifacts;
      doInstallCargoArtifacts = false;
      buildPhaseCargoCommand = ''
        cargo tree ${cargoManifestArgs} --workspace --target all --all-features --prefix none > "$TMPDIR/cuetty-dependencies"
        if grep -Eq '^proc-macro-error(-attr)?2? v' "$TMPDIR/cuetty-dependencies"; then
          echo "Cuetty must not depend on unmaintained proc-macro-error crates" >&2
          exit 1
        fi
        cargo clippy ${testFlags} -- ${clippyLints}
      '';
    }
  );
  cuetty-fmt = pkgs.stdenv.mkDerivation {
    pname = "cuetty-fmt";
    inherit version;
    src = fmtSrc;
    nativeBuildInputs = [ rustToolchain ];
    dontConfigure = true;
    buildPhase = ''
      ${rustfmtProbe}
      cargo fmt --manifest-path apps/cuetty/Cargo.toml --all -- --check --config ${rustfmtOptions}
    '';
    installPhase = "mkdir -p $out";
  };
in
{
  inherit
    package
    cuetty-test
    cuetty-clippy
    cuetty-fmt
    ;
  devShell = pkgs.mkShell {
    packages = [ rustToolchain ] ++ nativeBuildInputs ++ buildInputs;
    CUE_BRIDGE_PATH = cue-bridge;
  };
}
