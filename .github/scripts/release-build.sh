#!/usr/bin/env bash
set -euo pipefail

repo_root="$(pwd -P)"
case "${CUENV_ARCH:-}:$(uname -s):$(uname -m)" in
	linux-x64:Linux:x86_64)
		rust_target="x86_64-unknown-linux-gnu"
		zig_target="x86_64-linux-gnu.2.17"
		go_arch="amd64"
		interpreter="/lib64/ld-linux-x86-64.so.2"
		;;
	linux-arm64:Linux:aarch64)
		rust_target="aarch64-unknown-linux-gnu"
		zig_target="aarch64-linux-gnu.2.17"
		go_arch="arm64"
		interpreter="/lib/ld-linux-aarch64.so.1"
		;;
	darwin-arm64:Darwin:arm64)
		rust_target="aarch64-apple-darwin"
		zig_target="aarch64-macos.11.0"
		go_arch="arm64"
		;;
	*)
		echo "Unsupported release target or host: CUENV_ARCH=${CUENV_ARCH:-unset}, $(uname -s) $(uname -m)" >&2
		exit 1
		;;
esac

output="$repo_root/target/release/cuenv"
if [[ -e "$output" || -L "$output" ]]; then
	echo "Refusing to overwrite existing release output: $output" >&2
	exit 1
fi

# build.rs prefers workspace target/debug and target/release over the bridge
# override. Copy the declared sources into a fresh workspace so an existing
# checkout's target directory can never silently replace the release bridge.
temp_root="$(mktemp -d "${TMPDIR:-/tmp}/cuenv-release.XXXXXXXX")"
trap 'rm -rf -- "$temp_root"' EXIT
workspace="$temp_root/source"
mkdir -p "$workspace" "$temp_root/bin" "$workspace/target/release"

source_paths="$(jq -r '.files + .trees + .cueTrees | unique[]' nix/source-files.json)"
while IFS= read -r path; do
	if [[ "$path" == /* || "/$path/" == *"/../"* ]]; then
		echo "Invalid flake source path: $path" >&2
		exit 1
	fi
	mkdir -p "$workspace/$(dirname "$path")"
	rsync -a --exclude='target/' --exclude='.git/' --exclude='.direnv/' \
		"$repo_root/$path" "$workspace/$(dirname "$path")/" < /dev/null
done <<< "$source_paths"

zig="$(command -v zig)"
cat > "$temp_root/bin/zig-cc" <<EOF
#!/usr/bin/env bash
exec "$zig" cc -target "$zig_target" "\$@"
EOF
cat > "$temp_root/bin/zig-cxx" <<EOF
#!/usr/bin/env bash
exec "$zig" c++ -target "$zig_target" "\$@"
EOF
cat > "$temp_root/bin/zig-ar" <<EOF
#!/usr/bin/env bash
exec "$zig" ar "\$@"
EOF
chmod +x "$temp_root/bin/zig-cc" "$temp_root/bin/zig-cxx" "$temp_root/bin/zig-ar"

export CGO_ENABLED=1 GOARCH="$go_arch" GOTOOLCHAIN=local
export CC="$temp_root/bin/zig-cc" CXX="$temp_root/bin/zig-cxx" AR="$temp_root/bin/zig-ar"
export ZIG_GLOBAL_CACHE_DIR="$temp_root/zig-cache" ZIG_LOCAL_CACHE_DIR="$temp_root/zig-local-cache"
export MACOSX_DEPLOYMENT_TARGET=11.0
case "$CUENV_ARCH" in
	linux-*) export GOOS=linux ;;
	darwin-*) export GOOS=darwin ;;
esac

(
	cd "$workspace/crates/cuengine"
	mapfile -t go_sources < <(find . -maxdepth 1 -name '*.go' ! -name '*_test.go' -print | LC_ALL=C sort)
	if [[ ${#go_sources[@]} -eq 0 ]]; then
		echo "No Go bridge sources found" >&2
		exit 1
	fi
	go build -ldflags='-s -w' -buildmode=c-archive \
		-o "$workspace/target/release/libcue_bridge.a" "${go_sources[@]}"
)
cp "$workspace/crates/cuengine/libcue_bridge.h" "$workspace/target/release/libcue_bridge.h"

# Let cargo-zigbuild choose its own target compiler; do not inherit host
# linker, Rust wrapper, or flags from the caller's development environment.
unset CC CXX AR GOOS GOARCH CUE_BRIDGE_PATH RUSTC_WRAPPER RUSTFLAGS
unset CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_RUSTFLAGS
unset CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_RUSTFLAGS
export CARGO_TARGET_DIR="$temp_root/cargo-target"
export XDG_CACHE_HOME="$temp_root/xdg-cache" CARGO_ZIGBUILD_CACHE_DIR="$temp_root/zigbuild-cache"
cd "$workspace"
if [[ "$CUENV_ARCH" == linux-* ]]; then
	cargo zigbuild --release --locked --package cuenv --target "$rust_target.2.17"
	binary="$CARGO_TARGET_DIR/$rust_target/release/cuenv"
else
	cargo build --release --locked --package cuenv
	binary="$CARGO_TARGET_DIR/release/cuenv"
fi

if [[ "$CUENV_ARCH" == linux-* ]]; then
	headers="$(readelf --wide --program-headers "$binary")"
	dynamic="$(readelf --wide --dynamic "$binary")"
	if [[ "$headers" != *"[Requesting program interpreter: $interpreter]"* ]]; then
		echo "Release binary does not use the expected system interpreter: $interpreter" >&2
		exit 1
	fi
	if [[ "$headers$dynamic" == *"/nix/store/"* ]]; then
		echo "Release binary references a Nix store runtime dependency" >&2
		exit 1
	fi
	versions="$(readelf --wide --version-info "$binary" | grep -oE 'GLIBC_[[:alnum:]_.]+' | sort -u)"
	if [[ -z "$versions" ]]; then
		echo "Could not determine the release binary's glibc requirements" >&2
		exit 1
	fi
	while IFS= read -r version; do
		if [[ ! "$version" =~ ^GLIBC_([0-9]+\.[0-9]+(\.[0-9]+)*)$ ]]; then
			echo "Unexpected glibc requirement: $version" >&2
			exit 1
		fi
		if [[ "$(printf '%s\n' "${BASH_REMATCH[1]}" 2.17 | sort -V | tail -n 1)" != 2.17 ]]; then
			echo "Release binary requires $version, above glibc 2.17" >&2
			exit 1
		fi
	done <<< "$versions"
	runner=("${CUENV_RELEASE_LOADER:?Missing Nix glibc loader}" --library-path "${CUENV_RELEASE_GLIBC_LIB:?Missing Nix glibc library path}")
else
	libiconv="$(otool -L "$binary" | awk '/libiconv\.2\.dylib/ {print $1; exit}')"
	if [[ "$libiconv" == /nix/store/* ]]; then
		install_name_tool -change "$libiconv" /usr/lib/libiconv.2.dylib "$binary"
		# Changing Mach-O load commands invalidates the existing signature. Sign
		# a copy before putting it back, as Nix's Darwin fixup does.
		mkdir -p "$temp_root/signing"
		cp "$binary" "$temp_root/signing/cuenv"
		CODESIGN_ALLOCATE="$(command -v codesign_allocate)" codesign -f -s - "$temp_root/signing/cuenv"
		mv "$temp_root/signing/cuenv" "$binary"
	fi
	load_commands="$(otool -l "$binary")"
	if [[ "$load_commands" == *"/nix/store/"* || "$(otool -L "$binary")" == *"/nix/store/"* ]]; then
		echo "Release binary references a Nix store runtime dependency" >&2
		exit 1
	fi
	minimum="$(printf '%s\n' "$load_commands" | awk '
		$1 == "cmd" && ($2 == "LC_BUILD_VERSION" || $2 == "LC_VERSION_MIN_MACOSX") { in_version = 1; next }
		in_version && ($1 == "minos" || $1 == "version") { print $2; exit }
		in_version && $1 == "cmd" { in_version = 0 }
	')"
	if [[ ! "$minimum" =~ ^11\.0(\.0)?$ ]]; then
		echo "Release binary has unexpected macOS deployment target: ${minimum:-unknown}" >&2
		exit 1
	fi
	runner=()
fi

"${runner[@]}" "$binary" --version
"${runner[@]}" "$binary" env print --path examples/env-basic --package examples --output json \
	| jq -e '.DATABASE_URL == "postgres://localhost/mydb" and .PORT == "3000"' > /dev/null

mkdir -p "$(dirname "$output")"
# noclobber keeps direct invocation from overwriting an existing user build.
set -o noclobber
cat "$binary" > "$output"
chmod 755 "$output"
echo "Verified release binary: $output"
