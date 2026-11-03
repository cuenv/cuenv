# Offline lifecycle fixture: the exact official release archive, never a user
# provider installation. Verified against the v5.28.1 GitHub release asset digest
# and terraform-provider-cloudflare_5.28.1_SHA256SUMS on 2026-11-03.
{ pkgs }:
pkgs.stdenvNoCC.mkDerivation {
  pname = "cuenv-test-terraform-provider-cloudflare";
  version = "5.28.1";
  src = pkgs.fetchurl {
    url = "https://github.com/cloudflare/terraform-provider-cloudflare/releases/download/v5.28.1/terraform-provider-cloudflare_5.28.1_linux_amd64.zip";
    sha256 = "2ae200482bf592cd703981c4d4fe87a52c0dfae81a70261295f59b1bb0d01a6c";
  };
  nativeBuildInputs = [ pkgs.unzip ];
  dontUnpack = true;
  dontStrip = true;
  installPhase = ''
    runHook preInstall
    mkdir -p "$out/bin"
    unzip "$src" 'terraform-provider-cloudflare_v5.28.1' -d "$out/bin"
    chmod +x "$out/bin/terraform-provider-cloudflare_v5.28.1"
    runHook postInstall
  '';
  meta.platforms = [ "x86_64-linux" ];
}
