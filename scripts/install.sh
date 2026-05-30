#!/usr/bin/env bash
set -euo pipefail

version="${CUENV_VERSION:-latest}"
install_dir="${CUENV_INSTALL_DIR:-/usr/local/bin}"

os_name="$(uname -s)"
machine_name="$(uname -m)"

case "$os_name" in
	Darwin)
		platform="darwin"
		;;
	Linux)
		platform="linux"
		;;
	*)
		printf 'Unsupported operating system: %s\n' "$os_name" >&2
		exit 1
		;;
esac

case "$machine_name" in
	x86_64|amd64)
		arch="x64"
		;;
	aarch64|arm64)
		arch="arm64"
		;;
	*)
		printf 'Unsupported architecture: %s\n' "$machine_name" >&2
		exit 1
		;;
esac

asset="cuenv-${platform}-${arch}"
if [[ "$version" == "latest" ]]; then
	url="https://github.com/cuenv/cuenv/releases/latest/download/${asset}"
else
	url="https://github.com/cuenv/cuenv/releases/download/${version}/${asset}"
fi

tmpdir="$(mktemp -d)"
cleanup() {
	rm -rf "$tmpdir"
}
trap cleanup EXIT

downloaded="${tmpdir}/cuenv"

printf 'Downloading %s from %s\n' "$asset" "$url"
curl -fsSL -o "$downloaded" "$url"
chmod +x "$downloaded"

if [[ ! -d "$install_dir" ]]; then
	mkdir -p "$install_dir" 2>/dev/null || sudo mkdir -p "$install_dir"
fi

target="${install_dir}/cuenv"
if [[ -w "$install_dir" ]]; then
	install -m 0755 "$downloaded" "$target"
else
	sudo install -m 0755 "$downloaded" "$target"
fi

printf 'Installed cuenv to %s\n' "$target"