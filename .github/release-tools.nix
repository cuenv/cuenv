let
  lock = builtins.fromJSON (builtins.readFile ../flake.lock);
  input =
    name:
    let
      node = lock.nodes.${lock.nodes.${lock.root}.inputs.${name}};
    in
    (builtins.fetchTree (builtins.removeAttrs node.locked [ "lastModified" ])).outPath;

  system = builtins.currentSystem;
  pkgs = import (input "nixpkgs") {
    inherit system;
    overlays = [ (import (input "rust-overlay")) ];
  };
  unstable = import (input "nixpkgs-unstable") { inherit system; };

  # x86_64 Linux cross-compiles the linux-arm64 artifact. It only needs ARM's
  # glibc for the smoke test, which Nix substitutes instead of building.
  crossCompilesArm = system == "x86_64-linux";
  armGlibc = (import (input "nixpkgs") { system = "aarch64-linux"; }).glibc;
  nativeRust = pkgs.rust-bin.stable."1.98.1".default;
  rust =
    if crossCompilesArm then nativeRust.override { targets = [ "aarch64-unknown-linux-gnu" ]; } else nativeRust;
in
pkgs.mkShell {
  packages =
    with pkgs;
    [
      bash
      coreutils
      findutils
      git
      gawk
      gnugrep
      gnused
      go_1_26
      jq
      pkg-config
      rsync
      rust
      zig
    ]
    ++ lib.optionals stdenv.isLinux [
      binutils
      (unstable.cargo-zigbuild.override { inherit zig; })
    ]
    ++ lib.optionals crossCompilesArm [ qemu-user ]
    ++ lib.optionals stdenv.isDarwin [
      darwin.cctools
      darwin.sigtool
    ];

  buildInputs = pkgs.lib.optional pkgs.stdenv.isDarwin pkgs.libiconv;

  # Run the portable Linux executable on NixOS too, without changing its ELF
  # interpreter. This loader is used only for the release smoke test.
  CUENV_RELEASE_LOADER = pkgs.lib.optionalString pkgs.stdenv.isLinux pkgs.stdenv.cc.bintools.dynamicLinker;
  CUENV_RELEASE_GLIBC_LIB = pkgs.lib.optionalString pkgs.stdenv.isLinux "${pkgs.glibc}/lib";
  CUENV_RELEASE_ARM64_LOADER = pkgs.lib.optionalString crossCompilesArm "${armGlibc}/lib/ld-linux-aarch64.so.1";
  CUENV_RELEASE_ARM64_GLIBC_LIB = pkgs.lib.optionalString crossCompilesArm "${armGlibc}/lib";
  SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
}
