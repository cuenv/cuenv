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
        zigCCWrapper = pkgs.writeShellScriptBin "zig-cc" ''
          exec ${pkgs.zig}/bin/zig cc -target ${zigTarget} "$@"
        '';
        zigCXXWrapper = pkgs.writeShellScriptBin "zig-cxx" ''
          exec ${pkgs.zig}/bin/zig c++ -target ${zigTarget} "$@"
        '';
        zigARWrapper = pkgs.writeShellScriptBin "zig-ar" ''
          exec ${pkgs.zig}/bin/zig ar "$@"
        '';
        cue-bridge = pkgs.buildGoModule {
          pname = "libcue-bridge";
          inherit version;
          src = ../../crates/cuengine;
          vendorHash = "sha256-p8gfl2H0lThSmqIRQZWDYoQ3antrIslpCwRCNKQ1cKs=";
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
            export ZIG_GLOBAL_CACHE_DIR="$TMPDIR/zig-cache"
            export ZIG_LOCAL_CACHE_DIR="$TMPDIR/zig-local-cache"
            mkdir -p $out/debug $out/release
            go_sources=$(find . -maxdepth 1 -name '*.go' ! -name '*_test.go' -print | sort)
            go build -buildmode=c-archive -o $out/debug/libcue_bridge.a $go_sources
            cp libcue_bridge.h $out/debug/
            CGO_ENABLED=1 go build -ldflags="-s -w" -buildmode=c-archive -o $out/release/libcue_bridge.a $go_sources
            cp libcue_bridge.h $out/release/
            runHook postBuild
          '';
          installPhase = ''
            runHook preInstall
            runHook postInstall
          '';
        };
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
          cargoRoot = "apps/cuetty";
          buildAndTestSubdir = "apps/cuetty";
          doCheck = false;
          cargoLock = {
            lockFile = ./Cargo.lock;
            allowBuiltinFetchGit = true;
          };
          CUE_BRIDGE_PATH = cue-bridge;
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
