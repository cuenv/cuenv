{ pkgs, cue-bridge }:
let
  lib = pkgs.lib;
  rustToolchain = pkgs.rust-bin.stable."1.98.1".default.override {
    extensions = [
      "clippy"
      "rustfmt"
    ];
  };
  rustPlatform = pkgs.makeRustPlatform {
    cargo = rustToolchain;
    rustc = rustToolchain;
  };
  cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
  version = cargoToml.package.version;
  src = lib.cleanSourceWith {
    # Cuetty is an app-local workspace, but its Cuenv integration uses
    # the public `cuengine` crate from the repository. Package the
    # repository root so that Cargo can resolve that path dependency and
    # the workspace metadata it inherits.
    src = ../..;
    filter =
      path: type:
      let
        root = toString ../..;
        rel = lib.removePrefix "${root}/" (toString path);
      in
      !(
        lib.hasPrefix "target/" rel
        || lib.hasInfix "/target/" rel
        || lib.hasPrefix ".direnv/" rel
        || lib.hasInfix "/.direnv/" rel
        || lib.hasPrefix ".git/" rel
        || lib.hasInfix "/.git/" rel
        || lib.hasPrefix ".jj/" rel
        || lib.hasInfix "/.jj/" rel
        || rel == "result"
      );
  };
  xcodeXcrun = pkgs.writeShellScriptBin "xcrun" ''
    unset DEVELOPER_DIR SDKROOT
    exec /usr/bin/xcrun "$@"
  '';
  nativeBuildInputs = lib.optionals pkgs.stdenv.isDarwin [ xcodeXcrun ] ++ [ pkgs.pkg-config ];
  buildInputs =
    lib.optionals pkgs.stdenv.isDarwin [ pkgs.libiconv ]
    ++ lib.optionals pkgs.stdenv.isLinux [
      pkgs.alsa-lib
      pkgs.fontconfig
      pkgs.freetype
      pkgs.libxkbcommon
      pkgs.openssl
      pkgs.vulkan-loader
      pkgs.wayland
      pkgs.xorg.libxcb
      pkgs.xorg.libXcursor
      pkgs.xorg.libXi
      pkgs.xorg.libXrandr
    ];
  commonArgs = {
    pname = "cuetty";
    inherit
      version
      src
      nativeBuildInputs
      buildInputs
      ;
    cargoRoot = "apps/cuetty";
    buildAndTestSubdir = "apps/cuetty";
    doCheck = false;
    cargoLock = {
      lockFile = ./Cargo.lock;
      allowBuiltinFetchGit = true;
    };
    CUE_BRIDGE_PATH = cue-bridge;
  };
  package = rustPlatform.buildRustPackage (
    commonArgs
    // {
      cargoBuildFlags = [ "--bin=cuetty" ];
      nativeBuildInputs =
        nativeBuildInputs
        ++ lib.optionals pkgs.stdenv.isLinux [
          pkgs.patchelf
          pkgs.copyDesktopItems
        ];
      desktopItems = lib.optionals pkgs.stdenv.isLinux [
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
      postInstall = lib.optionalString pkgs.stdenv.isLinux ''
        install -Dm644 ${./assets/app/cuetty.svg} "$out/share/icons/hicolor/scalable/apps/cuetty.svg"
      '';
      postFixup = lib.optionalString pkgs.stdenv.isLinux ''
        # GPUI loads some libraries with dlopen, so keep their paths after
        # Nix's RPATH shrinking without exporting them to terminal children.
        patchelf --add-rpath ${lib.escapeShellArg (lib.makeLibraryPath buildInputs)} "$out/bin/cuetty"
      '';
      doInstallCheck = pkgs.stdenv.isLinux;
      installCheckPhase = lib.optionalString pkgs.stdenv.isLinux ''
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
  cuetty-test = rustPlatform.buildRustPackage (
    commonArgs
    // {
      pname = "cuetty-test";
      doCheck = true;
      checkPhase = "CUETTY_SKIP_PTY_TEST=1 cargo test --manifest-path apps/cuetty/Cargo.toml --locked --all-targets";
      installPhase = "mkdir -p $out";
    }
  );
  cuetty-clippy = rustPlatform.buildRustPackage (
    commonArgs
    // {
      pname = "cuetty-clippy";
      doCheck = false;
      buildPhase = "cargo clippy --manifest-path apps/cuetty/Cargo.toml --locked --all-targets --all-features -- -D warnings";
      installPhase = "mkdir -p $out";
    }
  );
  cuetty-fmt = pkgs.stdenv.mkDerivation {
    pname = "cuetty-fmt";
    inherit version src;
    nativeBuildInputs = [ rustToolchain ];
    dontConfigure = true;
    buildPhase = "cargo fmt --manifest-path apps/cuetty/Cargo.toml --all -- --check";
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
