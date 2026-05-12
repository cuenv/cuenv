#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

matrix="docs/design/specs/schema-coverage-matrix.md"
fail=0

trim() {
	local value="$1"
	value="${value#"${value%%[![:space:]]*}"}"
	value="${value%"${value##*[![:space:]]}"}"
	printf '%s' "$value"
}

record_failure() {
	printf 'schema-docs-check: %s\n' "$1" >&2
	fail=1
}

if [[ ! -f "$matrix" ]]; then
	record_failure "missing $matrix"
else
	row_count="$(awk '/^\| `schema\// { count++ } END { print count + 0 }' "$matrix")"
	if [[ "$row_count" -eq 0 ]]; then
		record_failure "$matrix has no schema rows"
	fi
fi

if [[ -f "$matrix" ]]; then
	while IFS=: read -r file _line rest; do
		def="$(sed -E 's/^([#A-Za-z0-9_]+).*/\1/' <<<"$rest")"
		if ! awk -F'|' -v file="$file" -v def="$def" '
			function clean(value) {
				gsub(/`/, "", value)
				gsub(/^[ \t]+|[ \t]+$/, "", value)
				return value
			}
			/^\| `schema\// {
				if (clean($2) == file && clean($3) == def) found = 1
			}
			END { exit found ? 0 : 1 }
		' "$matrix"; then
			record_failure "missing schema coverage row for $file $def"
		fi
	done < <(grep -rEn '^#[A-Za-z0-9_]+' schema)

	awk -F'|' '
		function clean(value) {
			gsub(/`/, "", value)
			gsub(/^[ \t]+|[ \t]+$/, "", value)
			return value
		}
		BEGIN {
			valid["implemented"] = 1
			valid["partial"] = 1
			valid["schema-only"] = 1
			valid["legacy"] = 1
			valid["internal"] = 1
			valid["docs-misleading"] = 1
			valid["needs-decision"] = 1
			errors = 0
		}
		/^\| `schema\// {
			for (i = 2; i <= 13; i++) {
				if (clean($i) == "") {
					printf("schema-docs-check: empty matrix cell on line %d column %d\n", NR, i) > "/dev/stderr"
					errors = 1
				}
			}
			status = clean($12)
			if (!(status in valid)) {
				printf("schema-docs-check: invalid status '%s' on line %d\n", status, NR) > "/dev/stderr"
				errors = 1
			}
		}
		END { exit errors ? 1 : 0 }
	' "$matrix" || fail=1
fi

required_skills=(
	cuenv-schema-first
	cuenv-project-env-secrets-hooks
	cuenv-tasks-graph-cache
	cuenv-services-images-runtime
	cuenv-tools-lock-vcs
	cuenv-generation-rules-formatting
	cuenv-ci-release
	cuenv-doc-drift-auditor
)

for skill in "${required_skills[@]}"; do
	skill_file=".agents/skills/$skill/SKILL.md"
	if [[ ! -f "$skill_file" ]]; then
		record_failure "missing $skill_file"
		continue
	fi
	if ! grep -q "^name: $skill\$" "$skill_file"; then
		record_failure "$skill_file has missing or wrong name frontmatter"
	fi
	if ! grep -qE '^description: .+' "$skill_file"; then
		record_failure "$skill_file is missing description frontmatter"
	fi
	if ! grep -q 'schema-coverage-matrix' "$skill_file"; then
		record_failure "$skill_file does not point agents at the schema coverage matrix"
	fi
	if ! grep -q 'schema/' "$skill_file"; then
		record_failure "$skill_file does not name its schema ownership"
	fi
done

if ! grep -q 'cuenv task ci\.schema-docs-check' AGENTS.md CLAUDE.md; then
	record_failure "AGENTS.md or CLAUDE.md must require cuenv task ci.schema-docs-check"
fi

stale_scope=(
	readme.md
	llms.txt
	prompts
)

stale_patterns=(
	'schema\.#Secret & \{ command'
	'schema\.#AWSSecretRef'
	'schema\.#VaultRef'
	'schema\.#NixFlake'
	'--output-format'
	'cuenv ci --generate'
	'cuenv sync ignore'
	'cuenv sync codeowners'
	'/how-to/cubes/'
	'/explanation/cuenv-cubes/'
)

for pattern in "${stale_patterns[@]}"; do
	if grep -rn -- "$pattern" "${stale_scope[@]}" >/tmp/cuenv-schema-docs-rg.out 2>/dev/null; then
		cat /tmp/cuenv-schema-docs-rg.out >&2
		record_failure "stale schema or command pattern matched: $pattern"
	fi
done

if [[ "$fail" -ne 0 ]]; then
	exit 1
fi

printf 'schema-docs-check: ok\n'
