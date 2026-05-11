{
  description = "Cuetty - a GPUI terminal app for cuenv backed by Ghostty VT";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.11";
    flake-utils.url = "github:numtide/flake-utils/v1.0.0";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # Keep this rev in lockstep with the `gpui_ghostty_terminal` git dep in Cargo.toml.
    # Both the Rust dep and this input must point at the same commit so the vendored
    # Ghostty VT source tree (consumed via the postPatch symlink below) matches the
    # generated bindings. Bumping either alone will yield a build that links the wrong
    # Zig artefacts.
    gpui-ghostty-src = {
      url = "git+https://github.com/Xuanwo/gpui-ghostty?rev=e3025981c6211dd7db2a825dc364ffb5d342f45e&submodules=1";
      flake = false;
    };
  };

  nixConfig = {
    extra-substituters = [
      "https://cache.nixos.org"
    ];
    experimental-features = [ "nix-command" "flakes" ];
    accept-flake-config = true;
  };

  outputs =
    inputs@{
      self,
      nixpkgs,
      flake-utils,
      rust-overlay,
      ...
    }:
    let
      systems = [
        "aarch64-darwin"
        "aarch64-linux"
        "x86_64-darwin"
        "x86_64-linux"
      ];
    in
    flake-utils.lib.eachSystem systems (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };

        rustToolchain = pkgs.rust-bin.stable."1.90.0".default.override {
          extensions = [
            "rust-src"
            "rust-analyzer"
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

        ziglyphPackage = pkgs.runCommandLocal "ziglyph-zig-package"
          {
            nativeBuildInputs = [ pkgs.zig_0_14 ];
            src = pkgs.fetchurl {
              url = "https://deps.files.ghostty.org/ziglyph-b89d43d1e3fb01b6074bc1f7fc980324b04d26a5.tar.gz";
              hash = "sha256-cse98+Ft8QUjX+P88yyYfaxJOJGQ9M7Ymw7jFxDz89k=";
            };
          }
          ''
            package_hash="$(zig fetch --global-cache-dir "$TMPDIR" "$src")"
            mv "$TMPDIR/p/$package_hash" "$out"
            chmod 755 "$out"
          '';

        ghosttyVtZigDeps = pkgs.linkFarm "ghostty-vt-zig-cache" [
          {
            name = "ziglyph-0.11.2-AAAAAHPtHwB4Mbzn1KvOV7Wpjo82NYEc_v0WC8oCLrkf";
            path = ziglyphPackage;
          }
        ];

        zigWithGhosttyDeps = pkgs.writeShellScriptBin "zig" ''
          tmp_root="''${TMPDIR:-/tmp}"
          export ZIG_GLOBAL_CACHE_DIR="''${ZIG_GLOBAL_CACHE_DIR:-$tmp_root/zig-global-cache}"
          export ZIG_LOCAL_CACHE_DIR="''${ZIG_LOCAL_CACHE_DIR:-$tmp_root/zig-local-cache}"
          mkdir -p "$ZIG_GLOBAL_CACHE_DIR" "$ZIG_LOCAL_CACHE_DIR"

          if [ "''${1:-}" = "build" ]; then
            shift
            exec ${pkgs.zig_0_14}/bin/zig build --system ${ghosttyVtZigDeps} "$@"
          fi

          exec ${pkgs.zig_0_14}/bin/zig "$@"
        '';

        src = pkgs.lib.cleanSourceWith {
          src = ./.;
          filter =
            path: type:
            let
              root = toString ./.;
              rel = pkgs.lib.removePrefix "${root}/" (toString path);
            in
            !(pkgs.lib.hasPrefix "termy/" rel || rel == "termy")
            && !(pkgs.lib.hasPrefix "target/" rel || rel == "target")
            && !(pkgs.lib.hasPrefix ".direnv/" rel || rel == ".direnv")
            && !(pkgs.lib.hasPrefix ".zig-cache/" rel || rel == ".zig-cache");
        };

        xcodeXcrun = pkgs.writeShellScriptBin "xcrun" ''
          unset DEVELOPER_DIR
          exec /usr/bin/xcrun "$@"
        '';

        commonNativeBuildInputs =
          pkgs.lib.optionals pkgs.stdenv.isDarwin [ xcodeXcrun ]
          ++ (with pkgs; [
            clang
            cmake
            git
            pkg-config
            zigWithGhosttyDeps
          ]);

        commonBuildInputs = with pkgs; [ libiconv ];

        linuxBuildInputs =
          with pkgs;
          lib.optionals stdenv.isLinux [
            alsa-lib
            fontconfig
            freetype
            libx11
            libxcb
            libxkbcommon
            openssl
            vulkan-loader
            wayland
            xorg.libXcursor
            xorg.libXi
            xorg.libXrandr
            zstd
          ];

        buildInputs = commonBuildInputs ++ linuxBuildInputs;

        commonPackageArgs = {
          pname = "cuetty";
          inherit version src buildInputs;

          cargoLock = {
            lockFile = ./Cargo.lock;
            allowBuiltinFetchGit = true;
          };

          nativeBuildInputs = commonNativeBuildInputs;

          postPatch = ''
            mkdir -p ../vendor
            ln -s ${inputs.gpui-ghostty-src}/vendor/ghostty ../vendor/ghostty
          '';

          env = {
            LIBCLANG_PATH = "${pkgs.libclang.lib}/lib";
            ZIG = "${zigWithGhosttyDeps}/bin/zig";
          };
        };

        cuetty = rustPlatform.buildRustPackage (
          commonPackageArgs
          // {
            cargoBuildFlags = [ "--bin=cuetty" ];
            doCheck = false;

            meta = {
              description = "A GPUI terminal app for cuenv backed by Ghostty VT";
              license = pkgs.lib.licenses.agpl3Only;
              mainProgram = "cuetty";
              platforms = systems;
            };
          }
        );

        cuetty-test = rustPlatform.buildRustPackage (
          commonPackageArgs
          // {
            pname = "cuetty-test";
            cargoBuildFlags = [ "--all-targets" ];
            doCheck = true;
            checkPhase = ''
              runHook preCheck
              cargo test --locked --all-targets
              runHook postCheck
            '';
            installPhase = ''
              runHook preInstall
              mkdir -p "$out"
              runHook postInstall
            '';
          }
        );

        cuetty-clippy = rustPlatform.buildRustPackage (
          commonPackageArgs
          // {
            pname = "cuetty-clippy";
            doCheck = false;
            buildPhase = ''
              runHook preBuild
              cargo clippy --locked --all-targets -- -D warnings
              runHook postBuild
            '';
            installPhase = ''
              runHook preInstall
              mkdir -p "$out"
              runHook postInstall
            '';
          }
        );

        cuetty-fmt = pkgs.stdenv.mkDerivation {
          pname = "cuetty-fmt";
          inherit version src;
          nativeBuildInputs = [ rustToolchain ];
          dontConfigure = true;
          buildPhase = ''
            runHook preBuild
            cargo fmt --all -- --check
            runHook postBuild
          '';
          installPhase = ''
            runHook preInstall
            mkdir -p "$out"
            runHook postInstall
          '';
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
          cuetty-build = cuetty;
          inherit cuetty-clippy cuetty-fmt cuetty-test;
        };

        devShells.default = pkgs.mkShell {
          packages =
            [
              rustToolchain
              pkgs.rust-analyzer
            ]
            ++ commonNativeBuildInputs
            ++ buildInputs;

          env = {
            LIBCLANG_PATH = "${pkgs.libclang.lib}/lib";
            ZIG = "${zigWithGhosttyDeps}/bin/zig";
          };
        };
      }
    );
}
