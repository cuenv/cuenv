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
      nativeBuildInputs = nativeBuildInputs ++ lib.optionals pkgs.stdenv.isLinux [ pkgs.makeWrapper ];
      postFixup = lib.optionalString pkgs.stdenv.isLinux ''
        wrapProgram $out/bin/cuetty \
          --prefix LD_LIBRARY_PATH : ${lib.makeLibraryPath buildInputs}
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
