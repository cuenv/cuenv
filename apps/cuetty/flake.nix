{
  description = "Cuetty, the Cuenv GPUI terminal backed by pinned Rio";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";
    flake-utils.url = "github:numtide/flake-utils/v1.0.0";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      nixpkgs,
      flake-utils,
      rust-overlay,
      ...
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };
        lib = pkgs.lib;
        rustToolchain = pkgs.rust-bin.stable."1.90.0".default.override {
          extensions = [ "clippy" "rustfmt" ];
        };
        rustPlatform = pkgs.makeRustPlatform {
          cargo = rustToolchain;
          rustc = rustToolchain;
        };
        cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
        version = cargoToml.package.version;
        src = lib.cleanSourceWith {
          src = ./.;
          filter =
            path: type:
            let
              root = toString ./.;
              rel = lib.removePrefix "${root}/" (toString path);
            in
            !(lib.hasPrefix "target/" rel || lib.hasPrefix ".direnv/" rel || rel == "result");
        };
        xcodeXcrun = pkgs.writeShellScriptBin "xcrun" ''
          unset DEVELOPER_DIR SDKROOT
          exec /usr/bin/xcrun "$@"
        '';
        nativeBuildInputs =
          lib.optionals pkgs.stdenv.isDarwin [ xcodeXcrun ]
          ++ [ pkgs.pkg-config ];
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
            pkgs.xorg.libXcursor
            pkgs.xorg.libXi
            pkgs.xorg.libXrandr
          ];
        commonArgs = {
          pname = "cuetty";
          inherit version src nativeBuildInputs buildInputs;
          cargoLock = {
            lockFile = ./Cargo.lock;
            allowBuiltinFetchGit = true;
          };
        };
        cuetty = rustPlatform.buildRustPackage (
          commonArgs
          // {
            cargoBuildFlags = [ "--bin=cuetty" ];
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
            checkPhase = "cargo test --locked --all-targets";
            installPhase = "mkdir -p $out";
          }
        );
        cuetty-clippy = rustPlatform.buildRustPackage (
          commonArgs
          // {
            pname = "cuetty-clippy";
            doCheck = false;
            buildPhase = "cargo clippy --locked --all-targets --all-features -- -D warnings";
            installPhase = "mkdir -p $out";
          }
        );
        cuetty-fmt = pkgs.stdenv.mkDerivation {
          pname = "cuetty-fmt";
          inherit version src;
          nativeBuildInputs = [ rustToolchain ];
          dontConfigure = true;
          buildPhase = "cargo fmt --all -- --check";
          installPhase = "mkdir -p $out";
        };
      in
      {
        packages = {
          default = cuetty;
          inherit cuetty;
        };
        apps = {
          default = flake-utils.lib.mkApp { drv = cuetty; };
          cuetty = flake-utils.lib.mkApp { drv = cuetty; };
        };
        checks = {
          inherit cuetty-test cuetty-clippy cuetty-fmt;
        };
        devShells.default = pkgs.mkShell {
          packages = [ rustToolchain ] ++ nativeBuildInputs ++ buildInputs;
        };
      }
    );
}
