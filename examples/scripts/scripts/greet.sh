#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "$0")/common.lib.sh"
greeting=hello
names=()
while [ "$#" -gt 0 ]; do
	case "$1" in
	--greeting)
		greeting="$2"
		shift 2
		;;
	*)
		names+=("$1")
		shift
		;;
	esac
done
for name in "${names[@]}"; do
	say "$greeting, $name"
done
