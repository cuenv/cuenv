#!/usr/bin/env bash
# Evaluate the working tree as a published schema at language v0.18.0.
# Compare accepted exports by value across configured CUE releases, and check
# rejected fixtures for their `// want: TEXT` diagnostics.
#
# Fixtures live under `testdata` so that CUE and cuenv's module-wide
# evaluation skip them; the reject fixtures are invalid on purpose.
#
# CUE releases come from CUENV_SCHEMA_COMPAT_CUE (space separated). Missing
# binaries are installed with `go install` into CUENV_SCHEMA_COMPAT_CACHE.
# Set CUENV_SCHEMA_COMPAT_KEEP to a directory to keep mismatching exports.
# Every evaluation is stopped after CUENV_SCHEMA_COMPAT_TIMEOUT seconds
# (default 60) and reported as a failure: a schema change can make
# evaluation explode instead of fail, which must not hang the check.
# Requires go, jq and coreutils' timeout.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."
repo="$(pwd)"

cue_versions="${CUENV_SCHEMA_COMPAT_CUE:-v0.18.0-rc.4}"
module_language="v0.18.0"
cache="${CUENV_SCHEMA_COMPAT_CACHE:-${XDG_CACHE_HOME:-${HOME:-/tmp}/.cache}/cuenv/schema-compat}"
eval_timeout="${CUENV_SCHEMA_COMPAT_TIMEOUT:-60}"

work="$(mktemp -d -t cuenv-schema-compat.XXXXXX)"
registry_pid=""
cleanup() {
	if [[ -n "$registry_pid" ]]; then
		kill "$registry_pid" 2>/dev/null || true
		wait "$registry_pid" 2>/dev/null || true
	fi
	# CUE extracts fetched modules read-only, like Go's module cache.
	chmod -R u+w "$work" 2>/dev/null || true
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
	if version_lt "$version" "$module_language"; then
		printf 'schema-compat: CUE %s does not support language %s\n' "$version" "$module_language" >&2
		exit 1
	fi
	cue[$version]="$(cue_bin "$version")"
	if [[ -z "$newest" ]] || version_lt "$newest" "$version"; then
		newest="$version"
	fi
done

# --- The module as a release publishes it ------------------------------------

# A release publishes every tracked file, tests included, and `cue mod publish`
# refuses a module that imports a package none of its dependencies provides.
# The copy published below leaves tests out, so check the module itself.
if ! CUE_REGISTRY=none "${cue[$newest]}" mod tidy --check >"$work/tidy.err" 2>&1; then
	record_failure "the module is not tidy, so a release cannot publish it: $(head -n2 "$work/tidy.err" | tr '\n' ' ')"
fi

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
language: version: "$module_language"
deps: "github.com/cuenv/cuenv@v0": v: "v0.0.1"
EOF
}

consumer() {
	local dest="$1" src="$2"
	rm -rf "$dest"
	mkdir -p "$dest/cue.mod"
	find "$repo/$src" -maxdepth 1 -name '*.cue' -not -name '.*' -exec cp {} "$dest/" \;
	if [[ -f "$repo/$src/.rules.cue" ]]; then
		cp "$repo/$src/.rules.cue" "$dest/rules.cue.txt"
	fi
	write_module "$dest"
}

# Exports are compared by value. CUE releases differ in the order they emit
# fields that come from imported definitions, and cuenv deserialises objects
# by key, so field order is not part of the schema contract.
#
# Evaluations that exceed eval_timeout fail with a timeout diagnostic.
evaluate() {
	local bin="$1" dir="$2" file="$3" out="$4" status=0
	(
		cd "$dir"
		if [[ -n "$file" ]]; then
			cp rules.cue.txt rules.cue
			exec timeout "$eval_timeout" "$bin" export --out json rules.cue
		else
			exec timeout "$eval_timeout" "$bin" export --out json .
		fi
	) >"$out.raw" 2>"$out.err" || status=$?
	if [[ "$status" -eq 124 ]]; then
		printf 'timed out after %ss (CUENV_SCHEMA_COMPAT_TIMEOUT): the schema makes this evaluation explode\n' \
			"$eval_timeout" >"$out.err"
		return 1
	fi
	[[ "$status" -eq 0 ]] || return 1
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
		if [[ "$mode" == "module" ]]; then
			dir="$module/$src"
		else
			dir="$work/fixture"
			consumer "$dir" "$src"
		fi
		out="$work/out.json"
		if ! evaluate "${cue[$version]}" "$dir" "$file" "$out"; then
			record_failure "$name: CUE $version: $(head -n3 "$out.err" | tr '\n' ' ')"
			continue
		fi
		if [[ -z "$baseline" ]]; then
			baseline="$work/baseline.json"
			cp "$out" "$baseline"
			baseline_version="$version"
		elif ! cmp -s "$baseline" "$out"; then
			record_failure "$name: export under CUE $version differs from $baseline_version"
			if [[ -n "${CUENV_SCHEMA_COMPAT_KEEP:-}" ]]; then
				keep="$CUENV_SCHEMA_COMPAT_KEEP/${name//\//_}"
				mkdir -p "$keep"
				cp "$baseline" "$keep/baseline.json"
				cp "$out" "$keep/$version.json"
			fi
		fi
	done
done

rejected=0
for dir in tests/schema-compat/testdata/reject/*/; do
	[[ -d "$dir" ]] || continue
	rejected=$((rejected + 1))
	name="${dir%/}"
	want="$(sed -n 's|^// want: ||p' "$dir"*.cue | head -n1)"
	if [[ -z "$want" ]]; then
		record_failure "$name: missing '// want:' line"
		continue
	fi
	for version in $cue_versions; do
		consumer "$work/fixture" "$name"
		out="$work/out.json"
		if evaluate "${cue[$version]}" "$work/fixture" "" "$out"; then
			record_failure "$name: accepted under CUE $version; want error containing '$want'"
		elif ! grep -qF -- "$want" "$out.err"; then
			record_failure "$name: CUE $version failed without '$want': $(head -n2 "$out.err" | tr '\n' ' ')"
		fi
	done
done

# --- Multi-directory consumer modules ---------------------------------------
#
# tests/schema-compat/testdata/modules/NAME/ is the tree of a whole consumer
# module: several projects, bases, and imports between them
# (`example.com/consumer/<dir>:cuenv`). Every directory holding CUE files
# must export, and export the same under every release.
# tests/schema-compat/testdata/modules-reject/NAME/ is the same, except that
# a directory whose files carry a `// want: TEXT` line must fail with it.
# The files of both are not named env.cue: cuenv discovers every env.cue of
# this repository as a project of its own module. Each fixture has its own
# cue.mod/module.cue, so its imports of `example.com/consumer` are never part
# of this module; the copy evaluated here gets the module written below.

module_fixture() {
	local dest="$1" src="$2"
	rm -rf "$dest"
	mkdir -p "$dest/cue.mod"
	while IFS= read -r file; do
		mkdir -p "$dest/$(dirname "$file")"
		cp "$repo/$src/$file" "$dest/$file"
	done < <(cd "$repo/$src" && find . -name '*.cue' -not -name '.*' | sed 's|^\./||')
	write_module "$dest"
}

# The directories of a module fixture that hold CUE files, `.` for the root.
module_directories() {
	(cd "$1" && find . -name '*.cue' -not -path './cue.mod/*' -exec dirname {} \; | sed 's|^\./||' | sort -u)
}

module_fixtures=0
for dir in tests/schema-compat/testdata/modules/*/ tests/schema-compat/testdata/modules-reject/*/; do
	[[ -d "$dir" ]] || continue
	module_fixtures=$((module_fixtures + 1))
	name="${dir%/}"
	if [[ "$name" == *modules-reject/* ]] && ! grep -rqs '^// want: ' "$dir"; then
		record_failure "$name: a rejected module needs a '// want:' line"
		continue
	fi
	declare -A module_baselines=()
	for version in $cue_versions; do
		fixture="$work/module-fixture"
		module_fixture "$fixture" "$name"
		while IFS= read -r sub; do
			out="$work/out.json"
			want="$(sed -n 's|^// want: ||p' "$fixture/$sub"/*.cue | head -n1)"
			if [[ -n "$want" ]]; then
				if evaluate "${cue[$version]}" "$fixture/$sub" "" "$out"; then
					record_failure "$name/$sub: accepted under CUE $version, language $module_language; want error containing '$want'"
				elif ! grep -qF -- "$want" "$out.err"; then
					record_failure "$name/$sub: CUE $version, language $module_language failed without '$want': $(head -n2 "$out.err" | tr '\n' ' ')"
				fi
				continue
			fi
			if ! evaluate "${cue[$version]}" "$fixture/$sub" "" "$out"; then
				record_failure "$name/$sub: CUE $version, language $module_language: $(head -n3 "$out.err" | tr '\n' ' ')"
				continue
			fi
			if [[ -z "${module_baselines[$sub]:-}" ]]; then
				module_baselines[$sub]="$work/module-baseline-${#module_baselines[@]}.json"
				cp "$out" "${module_baselines[$sub]}"
			elif ! cmp -s "${module_baselines[$sub]}" "$out"; then
				record_failure "$name/$sub: export under CUE $version, language $module_language differs from an earlier release"
			fi
		done < <(module_directories "$fixture")
	done
	unset module_baselines
done

if [[ "$fail" -ne 0 ]]; then
	exit 1
fi
printf 'schema-compat: %d fixtures accepted, %d rejected and %d modules checked under CUE %s\n' \
	"${#fixtures[@]}" "$rejected" "$module_fixtures" "$cue_versions"
