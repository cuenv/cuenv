#!/usr/bin/env bash
set -euo pipefail
source "$(dirname "$0")/common.lib.sh"
for argument in "$@"; do
	say "argument: $argument"
done
