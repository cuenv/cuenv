#!/usr/bin/env bash
# Evaluate the cuenv schema the way consumers do, across CUE releases and
# module language versions.
#
# The working tree is published to an in-memory CUE registry, and each fixture
# is copied into its own consumer module that depends on it. Every fixture is
# then evaluated with each configured CUE release and each module language
# version that release supports:
#
# - examples/**  must evaluate, and must export the same value under every
#   (CUE, language) pair.
# - tests/schema-compat/testdata/accept/NAME/  same contract, for focused
#   fixtures.
# - tests/schema-compat/testdata/reject/NAME/  must fail. A `// want: TEXT`
#   line names the expected error text. A `// min-language: vX.Y.Z` line
#   skips language versions that cannot reject it.
#
# Fixtures live under `testdata` so that CUE and cuenv's module-wide
# evaluation skip them; the reject fixtures are invalid on purpose.
#
# CUE releases come from CUENV_SCHEMA_COMPAT_CUE (space separated). Missing
# binaries are installed with `go install` into CUENV_SCHEMA_COMPAT_CACHE.
# Set CUENV_SCHEMA_COMPAT_KEEP to a directory to keep mismatching exports.
# Requires go and jq.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
repo="$(pwd)"

cue_versions="${CUENV_SCHEMA_COMPAT_CUE:-v0.16.1 v0.17.1 v0.18.0-rc.1}"
language_versions="v0.9.0 v0.14.1 v0.16.0 v0.17.0 v0.18.0"
cache="${CUENV_SCHEMA_COMPAT_CACHE:-${XDG_CACHE_HOME:-${HOME:-/tmp}/.cache}/cuenv/schema-compat}"

work="$(mktemp -d -t cuenv-schema-compat.XXXXXX)"
registry_pid=""
cleanup() {
	if [[ -n "$registry_pid" ]]; then
		kill "$registry_pid" 2>/dev/null || true
		wait "$registry_pid" 2>/dev/null || true
	fi
	rm -rf "$work"
}
trap cleanup EXIT

fail=0
record_failure() {
	printf 'schema-compat: %s\n' "$1" >&2
	fail=1
}

# Sortable key for vMAJOR.MINOR.PATCH, ignoring any pre-release suffix.
version_key() {
	local v="${1#v}"
	v="${v%%-*}"
	IFS=. read -r major minor patch <<<"$v"
	printf '%03d%03d%03d' "$major" "$minor" "$patch"
}

version_lt() {
	[[ "$(version_key "$1")" < "$(version_key "$2")" ]]
}

cue_bin() {
	local version="$1" dir="$cache/$1"
	if [[ ! -x "$dir/cue" ]]; then
		mkdir -p "$dir"
		GOBIN="$dir" GOTOOLCHAIN=auto go install "cuelang.org/go/cmd/cue@$version" >&2
	fi
	printf '%s' "$dir/cue"
}

declare -A cue
newest=""
for version in $cue_versions; do
	cue[$version]="$(cue_bin "$version")"
	if [[ -z "$newest" ]] || version_lt "$newest" "$version"; then
		newest="$version"
	fi
done

# A CUE release evaluates modules up to its own language version.
supports_language() {
	! version_lt "$1" "$2"
}

# --- Publish the working tree to an in-memory registry ----------------------

# Create the log first: the background job opens it asynchronously.
: >"$work/registry.log"
"${cue[$newest]}" mod registry 127.0.0.1:0 >"$work/registry.log" 2>&1 &
registry_pid=$!
address=""
for _ in $(seq 1 100); do
	address="$(sed -n 's/^listening on //p' "$work/registry.log")"
	[[ -n "$address" ]] && break
	sleep 0.1
done
if [[ -z "$address" ]]; then
	printf 'schema-compat: registry did not start: %s\n' "$(cat "$work/registry.log")" >&2
	exit 1
fi
export CUE_REGISTRY="$address+insecure"
export CUE_CACHE_DIR="$work/cue-cache"

# Every CUE file of the module, as a consumer would fetch it.
module="$work/module"
mkdir -p "$module/cue.mod"
while IFS= read -r file; do
	mkdir -p "$module/$(dirname "$file")"
	cp "$file" "$module/$file"
done < <(find . -name '*.cue' \
	-not -path './cue.mod/*' -not -path './.git/*' -not -path './tests/*' \
	-not -path './target/*' -not -path '*/node_modules/*' | sed 's|^\./||')
sed 's/kind: "git"/kind: "self"/' cue.mod/module.cue >"$module/cue.mod/module.cue"
(cd "$module" && "${cue[$newest]}" mod publish v0.0.1 >/dev/null)

# --- Fixtures ----------------------------------------------------------------

# Each fixture is "NAME|SOURCE DIR|FILE". A non-empty FILE is evaluated on
# its own, the way cuenv loads `.rules.cue`.
fixtures=()
while IFS= read -r dir; do
	fixtures+=("$dir|$dir|")
done < <(find examples -name '*.cue' -not -name '.*' -exec dirname {} \; | sort -u)
while IFS= read -r file; do
	fixtures+=("$file|$(dirname "$file")|$(basename "$file")")
done < <(find examples -name '.rules.cue' | sort)
for dir in tests/schema-compat/testdata/accept/*/; do
	[[ -d "$dir" ]] && fixtures+=("${dir%/}|${dir%/}|")
done

write_module() {
	cat >"$1/cue.mod/module.cue" <<EOF
module: "example.com/consumer"
language: version: "$2"
deps: "github.com/cuenv/cuenv@v0": v: "v0.0.1"
EOF
}

consumer() {
	local dest="$1" src="$2" language="$3"
	rm -rf "$dest"
	mkdir -p "$dest/cue.mod"
	find "$repo/$src" -maxdepth 1 -name '*.cue' -not -name '.*' -exec cp {} "$dest/" \;
	if [[ -f "$repo/$src/.rules.cue" ]]; then
		cp "$repo/$src/.rules.cue" "$dest/rules.cue.txt"
	fi
	write_module "$dest" "$language"
	# Language v0.18 rejects prefix aliases. That is a syntax change in the
	# consumer's own files, not a schema concern, so rewrite them the way an
	# upgrading consumer would: run the fix under v0.17, then move to v0.18.
	# Never apply explicitopen here: its `schema.#Project...` rewrite turns
	# validation off, and consumers are told not to accept it.
	if ! version_lt "$language" v0.18.0 && [[ -n "$(find "$dest" -maxdepth 1 -name '*.cue' -print -quit)" ]]; then
		write_module "$dest" v0.17.0
		if ! (cd "$dest" && "${cue[$newest]}" fix --exp=aliasv2 . >"$dest/fix.log" 2>&1); then
			record_failure "$src: cue fix --exp=aliasv2 failed: $(head -n2 "$dest/fix.log" | tr '\n' ' ')"
		fi
		write_module "$dest" "$language"
	fi
}

# Exports are compared by value. CUE releases differ in the order they emit
# fields that come from imported definitions, and cuenv deserialises objects
# by key, so field order is not part of the schema contract.
evaluate() {
	local bin="$1" dir="$2" file="$3" out="$4"
	(
		cd "$dir"
		if [[ -n "$file" ]]; then
			cp rules.cue.txt rules.cue
			"$bin" export --out json rules.cue
		else
			"$bin" export --out json .
		fi
	) >"$out.raw" 2>"$out.err" || return 1
	jq -S . <"$out.raw" >"$out"
}

# A fixture that imports another package of the cuenv module by its module
# path (examples/imported-task-dirs) is evaluated inside the published copy.
needs_module_path() {
	grep -qE '"github.com/cuenv/cuenv/examples/' "$repo/$1"/*.cue 2>/dev/null
}

for entry in "${fixtures[@]}"; do
	IFS='|' read -r name src file <<<"$entry"
	mode="consumer"
	if [[ -z "$file" ]] && needs_module_path "$src"; then
		mode="module"
	fi
	baseline=""
	for version in $cue_versions; do
		for language in $language_versions; do
			supports_language "$version" "$language" || continue
			if [[ "$mode" == "module" ]]; then
				# The module pins its own language version; vary only CUE.
				[[ "$language" == "v0.14.1" ]] || continue
				dir="$module/$src"
			else
				dir="$work/fixture"
				consumer "$dir" "$src" "$language"
			fi
			out="$work/out.json"
			if ! evaluate "${cue[$version]}" "$dir" "$file" "$out"; then
				record_failure "$name: CUE $version, language $language: $(head -n3 "$out.err" | tr '\n' ' ')"
				continue
			fi
			if [[ -z "$baseline" ]]; then
				baseline="$work/baseline.json"
				cp "$out" "$baseline"
				baseline_label="CUE $version, language $language"
			elif ! cmp -s "$baseline" "$out"; then
				record_failure "$name: export under CUE $version, language $language differs from $baseline_label"
				if [[ -n "${CUENV_SCHEMA_COMPAT_KEEP:-}" ]]; then
					keep="$CUENV_SCHEMA_COMPAT_KEEP/${name//\//_}"
					mkdir -p "$keep"
					cp "$baseline" "$keep/baseline.json"
					cp "$out" "$keep/$version-$language.json"
				fi
			fi
		done
	done
done

rejected=0
for dir in tests/schema-compat/testdata/reject/*/; do
	[[ -d "$dir" ]] || continue
	rejected=$((rejected + 1))
	name="${dir%/}"
	want="$(sed -n 's|^// want: ||p' "$dir"*.cue | head -n1)"
	min_language="$(sed -n 's|^// min-language: ||p' "$dir"*.cue | head -n1)"
	if [[ -z "$want" ]]; then
		record_failure "$name: missing '// want:' line"
		continue
	fi
	for version in $cue_versions; do
		for language in $language_versions; do
			supports_language "$version" "$language" || continue
			if [[ -n "$min_language" ]] && version_lt "$language" "$min_language"; then
				continue
			fi
			consumer "$work/fixture" "$name" "$language"
			out="$work/out.json"
			if evaluate "${cue[$version]}" "$work/fixture" "" "$out"; then
				record_failure "$name: accepted under CUE $version, language $language; want error containing '$want'"
			elif ! grep -qF -- "$want" "$out.err"; then
				record_failure "$name: CUE $version, language $language failed without '$want': $(head -n2 "$out.err" | tr '\n' ' ')"
			fi
		done
	done
done

if [[ "$fail" -ne 0 ]]; then
	exit 1
fi
printf 'schema-compat: %d fixtures accepted and %d rejected under CUE %s\n' \
	"${#fixtures[@]}" "$rejected" "$cue_versions"
