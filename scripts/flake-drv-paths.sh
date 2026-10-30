#!/usr/bin/env bash
# Write the derivation path of every package and check the flake defines for
# this system to OUTPUT, one `<attribute> <drv path>` line each, sorted.
#
# `ci.flake-drv-paths` runs this inside a task exec root, which holds only the
# files env.cue declares as the flake's inputs; `ci.flake-source-check` runs it
# again in the checkout and compares. Equal lists mean a task that runs the
# flake builds the very store paths CI already built and cached.
set -euo pipefail

if [ "$#" -ne 1 ]; then
	echo "usage: $0 OUTPUT" >&2
	exit 2
fi
output="$1"

system="$(nix eval --impure --raw --expr builtins.currentSystem)"

drv_paths() {
	nix eval --raw --accept-flake-config ".#$1.$system" --apply "outputs:
		builtins.concatStringsSep \"\" (builtins.attrValues (builtins.mapAttrs
			(name: drv: \"$1.\${name} \${drv.drvPath}\n\") outputs))"
}

mkdir -p "$(dirname "$output")"
{
	drv_paths packages
	drv_paths checks
} | LC_ALL=C sort >"$output"
