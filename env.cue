package cuenv

import (
	"list"

	"github.com/cuenv/cuenv/schema"
	xCodecov "github.com/cuenv/cuenv/contrib/codecov"
	xContributors "github.com/cuenv/cuenv/contrib/contributors"
	xNix "github.com/cuenv/cuenv/contrib/nix"
)

// Command template for cargo tasks
#cargo: schema.#Task & {command: "cargo"}

// Shared input patterns for Rust tasks
let _baseInputs = [
	"Cargo.toml",
	"Cargo.lock",
	"crates/**",
]

let _checkInputs = list.Concat([
	["flake.nix", "flake.lock"],
	_baseInputs,
	["_tests/**", "contrib/**", "examples/**", "schema/**", "cue.mod/**", ".agents/skills/**", "deny.toml", "env.cue"],
])

let _schemaDocsInputs = [
	"AGENTS.md",
	"readme.md",
	"llms.txt",
	"env.cue",
	"schema/**",
	"docs/design/specs/schema-coverage-matrix.md",
	"docs/src/content/docs/decisions/adrs/adr-0006-library-first-provider-system.md",
	"docs/src/content/docs/decisions/adrs/adr-0009-single-internal-sync-registry.md",
	"docs/src/content/docs/explanation/**",
	"docs/src/content/docs/reference/**",
	"docs/src/content/docs/agents/**",
	"docs/src/content/docs/index.mdx",
	"docs/src/content/docs/tutorials/**",
	"docs/src/content/docs/how-to/**",
	".agents/skills/**",
	"prompts/**",
	"scripts/check-schema-docs.sh",
]

schema.#Project & {
	name: "cuenv"

	// Alias to avoid scoping conflict with pipeline's tasks field
	let _t = tasks

	// ============================================================================
	// Runtime Configuration
	// ============================================================================

	runtime: schema.#NixRuntime

	// ============================================================================
	// Hooks & Formatters
	// ============================================================================

	hooks: onEnter: nix: xNix.#NixFlake

	formatters: rust: {edition: "2024"}

	// ============================================================================
	// Configuration
	// ============================================================================

	// Build cuenv from the checked-out repository flake in CI when the workflow itself
	// needs cuenv (for example sync-check and release orchestration).
	config: ci: cuenv: {source: "nix", version: "self"}

	// ============================================================================
	// Environment Variables
	// ============================================================================

	env: {
		CLOUDFLARE_ACCOUNT_ID: "0aeb879de8e3cdde5fb3d413025222ce"

		environment: production: {
			CLOUDFLARE_API_TOKEN: schema.#OnePasswordRef & {ref: "op://cuenv-github/cloudflare/password"}
			CODECOV_TOKEN: schema.#OnePasswordRef & {ref: "op://cuenv-github/codecov/password"}
			VSCE_PAT: schema.#OnePasswordRef & {ref: "op://cuenv-github/visual-studio-code/password"}
		}
	}

	// ============================================================================
	// CI Configuration
	// ============================================================================

	ci: {
		// Emit workflows for GitHub Actions only
		providers: ["github"]

		contributors: [
			xContributors.#Nix,
			xContributors.#Hestia,
			xContributors.#CuenvNix,
			xContributors.#OnePassword,
			xCodecov.#Codecov,
			xContributors.#CueRegistryTrustedPublishing,
		]

		provider: github: {
			runner: "namespace-profile-cuenv-linux-x86"

			runners: arch: {
				"linux-x64":    "namespace-profile-cuenv-linux-x86"
				"linux-arm64":  "namespace-profile-cuenv-linux-arm64"
				"darwin-arm64": "namespace-profile-cuenv-macos-arm64"
			}

			hestia: {}

			trustedPublishing: cueRegistry: true

			artifacts: {
				paths: [".cuenv/reports/"]
				ifNoFilesFound: "ignore"
			}
		}

		pipelines: {
			"sync-check": {
				when: {
					branch:      "main"
					pullRequest: true
				}
				provider: github: permissions: "id-token": "write"
				tasks: [
					_t.ci."sync-check",
					_t.ci."schema-docs-check",
				]
			}

			ci: {
				mode: "expanded"
				when: {
					branch:      "main"
					pullRequest: true
				}
				provider: github: permissions: "id-token": "write"
				tasks: [_t.checks]
			}

			release: {
				environment: "production"
				when: {
					release: ["published"]
					manual: tag_name: {
						description: "Tag to release (e.g., v0.16.0)"
						required:    true
						type:        "string"
					}
				}
				provider: github: permissions: {
					contents:   "write"
					"id-token": "write"
				}
				tasks: [
					{
						task:   _t.cargo.build
						matrix: arch: ["linux-x64", "linux-arm64", "darwin-arm64"]
					},
					{
						task: _t.publish.github
						artifacts: [{
							from:   "cargo.build"
							to:     "dist"
							filter: ""
						}]
						params: {
							tag:   "${{ github.ref_name }}"
							paths: "dist/**/*"
						}
					},
					_t.publish.cue,
					_t.publish.homebrew,
					_t.docs.deploy,
				]
			}
		}
	}

	// ============================================================================
	// Tasks
	// ============================================================================

	tasks: {
		// --- CI Internal ---
		ci: {
			type: "group"

			"sync-check": schema.#Task & {
				command: "cuenv"
				args: ["sync", "--check"]
				inputs: ["env.cue", "schema/**", "contrib/**", "cuenv.lock"]
			}

			"schema-docs-check": schema.#Task & {
				command: "bash"
				args: ["scripts/check-schema-docs.sh"]
				inputs: _schemaDocsInputs
			}
		}

		// --- CI Check (flake-owned lint, tests, security) ---
		check: schema.#Task & {
			command: "nix"
			args: ["flake", "check", "-L", "--accept-flake-config"]
			inputs: _checkInputs
		}

		checks: {
			type: "group"

			cuenv: schema.#Task & {
				command: "nix"
				args: ["build", ".#checks.x86_64-linux.cuenv", "-L", "--accept-flake-config"]
				inputs: _checkInputs
			}

			audit: schema.#Task & {
				command: "nix"
				args: ["build", ".#checks.x86_64-linux.cuenv-audit", "-L", "--accept-flake-config"]
				inputs: _checkInputs
			}

			bdd: schema.#Task & {
				command: "nix"
				args: ["build", ".#checks.x86_64-linux.cuenv-bdd", "-L", "--accept-flake-config"]
				inputs: _checkInputs
			}

			clippy: schema.#Task & {
				command: "nix"
				args: ["build", ".#checks.x86_64-linux.cuenv-clippy", "-L", "--accept-flake-config"]
				inputs: _checkInputs
			}

			deny: schema.#Task & {
				command: "nix"
				args: ["build", ".#checks.x86_64-linux.cuenv-deny", "-L", "--accept-flake-config"]
				inputs: _checkInputs
			}

			doctest: schema.#Task & {
				command: "nix"
				args: ["build", ".#checks.x86_64-linux.cuenv-doctest", "-L", "--accept-flake-config"]
				inputs: _checkInputs
			}

			nextest: schema.#Task & {
				command: "nix"
				args: ["build", ".#checks.x86_64-linux.cuenv-nextest", "-L", "--accept-flake-config"]
				inputs: _checkInputs
			}
		}

		// --- Linting ---
		lint: #cargo & {
			args: ["clippy", "--workspace", "--all-targets", "--all-features", "--", "-D", "warnings"]
			inputs: _baseInputs
			dependsOn: [cargo.compile]
		}

		// --- Testing ---
		tests: {
			type: "group"
			dependsOn: [cargo.compile]

			unit: #cargo & {
				args: ["nextest", "run", "--workspace", "--all-features"]
				inputs: list.Concat([_baseInputs, ["_tests/**", "examples/**", "schema/**", "cue.mod/**", ".agents/skills/**"]])
			}

			doc: #cargo & {
				args: ["test", "--doc", "--workspace"]
				inputs: _baseInputs
			}

			bdd: #cargo & {
				args: ["test", "--test", "bdd"]
				inputs: list.Concat([_baseInputs, ["_tests/**", "schema/**", "cue.mod/**"]])
				outputs: [".test"]
			}
		}

		// --- Security & Quality ---
		security: {
			type: "group"

			deny: #cargo & {
				args: ["deny", "check", "bans", "licenses"]
				inputs: list.Concat([_baseInputs, ["deny.toml"]])
			}

			audit: schema.#Task & {
				script: """
					#!/usr/bin/env bash
					set -euo pipefail

					audit_db="$(mktemp -d "${TMPDIR:-/tmp}/cuenv-audit-db.XXXXXX")"
					trap 'rm -rf "$audit_db"' EXIT

					git clone --quiet https://github.com/RustSec/advisory-db.git "$audit_db"
					find "$audit_db" -name '*.md' -exec sed -E -i.bak '/^cvss = "CVSS:4\\.0\\//d' {} +
					find "$audit_db" -name '*.bak' -delete

					cargo audit --db "$audit_db" --no-fetch --deny warnings --ignore RUSTSEC-2023-0071 --ignore RUSTSEC-2025-0057 --ignore RUSTSEC-2025-0134 --ignore RUSTSEC-2026-0006 --ignore RUSTSEC-2026-0020 --ignore RUSTSEC-2026-0021 --ignore RUSTSEC-2026-0037
					"""
				inputs: list.Concat([_baseInputs, ["Cargo.lock"]])
			}
		}

		sbom: #cargo & {
			args: ["cyclonedx", "--override-filename", "sbom.json"]
			inputs: _baseInputs
			outputs: ["sbom.json"]
		}

		coverage: #cargo & {
			args: ["llvm-cov", "nextest", "--workspace", "--all-features", "--lcov", "--output-path", "lcov.info"]
			inputs: list.Concat([_baseInputs, ["_tests/**", "examples/**", "schema/**", "cue.mod/**", ".agents/skills/**"]])
			outputs: ["lcov.info"]
			labels: ["coverage"]
		}

		// --- Benchmarks ---
		bench: #cargo & {
			args: ["bench", "--workspace", "--no-fail-fast"]
			inputs: _baseInputs
		}

		// --- Documentation ---
		docs: {
			type: "group"

			build: schema.#Task & {
				hermetic: {passthrough: ["PATH"]}
				command: "bash"
				args: ["-c", """
					set -euo pipefail
					mkdir -p .cuenv-home .cuenv-cache .cuenv-tmp
					task_root="$PWD"
					trap 'rm -rf "$task_root/.cuenv-home" "$task_root/.cuenv-cache" "$task_root/.cuenv-tmp"' EXIT
					export HOME="$task_root/.cuenv-home"
					export XDG_CACHE_HOME="$task_root/.cuenv-cache"
					export TMPDIR="$task_root/.cuenv-tmp"
					export TMP="$TMPDIR"
					export TEMP="$TMPDIR"

					bun install --frozen-lockfile
					cd docs
					bun run build
					cp public/.assetsignore dist/
					"""]
				inputs: [
					"package.json",
					"bun.lock",
					"integrations/*/package.json",
					"docs/**",
				]
				outputs: ["docs/dist"]
			}

			deploy: schema.#Task & {
				// The task output carries docs/dist; this adds Wrangler's config.
				hermetic: {passthrough: ["PATH"]}
				command: "bash"
				args: ["-c", """
					set -euo pipefail
					mkdir -p .cuenv-home .cuenv-cache .cuenv-tmp
					task_root="$PWD"
					trap 'rm -rf "$task_root/.cuenv-home" "$task_root/.cuenv-cache" "$task_root/.cuenv-tmp"' EXIT
					export HOME="$task_root/.cuenv-home"
					export XDG_CACHE_HOME="$task_root/.cuenv-cache"
					export TMPDIR="$task_root/.cuenv-tmp"
					export TMP="$TMPDIR"
					export TEMP="$TMPDIR"

					bun install --frozen-lockfile
					cd docs
					bunx --no-install wrangler deploy
					"""]
				dependsOn: [_t.docs.build]
				inputs: [
					{task: "docs.build"},
					"package.json",
					"bun.lock",
					"integrations/*/package.json",
					"docs/package.json",
					"docs/wrangler.jsonc",
				]
			}
		}

		// --- Build & Release ---
		cargo: {
			type: "group"

			compile: #cargo & {
				args: ["build", "--workspace", "--all-targets", "--all-features"]
				inputs: _baseInputs
			}

			build: schema.#Task & {
				hermetic: {passthrough: ["PATH"]}
				script: """
					#!/usr/bin/env bash
					set -euo pipefail

					echo "Building release artifact from flake output..."
					nix build .#cuenv -L --accept-flake-config
					mkdir -p target/release
					cp result/bin/cuenv target/release/cuenv

					echo "Binary at: target/release/cuenv"
					file target/release/cuenv
					"""
				inputs: list.Concat([_baseInputs, ["flake.nix", "flake.lock"]])
				outputs: ["target/release/cuenv"]
			}

			quick: #cargo & {
				args: ["build", "--workspace", "--all-features", "--profile", "quick"]
				inputs: _baseInputs
			}

			install: #cargo & {
				args: ["install", "--path", "./crates/cuenv"]
				inputs: _baseInputs
			}
		}

		cross: {
			type: "group"

			linux: schema.#Task & {
				hermetic: {passthrough: ["PATH"]}
				command: "nix"
				args: ["build", ".#cuenv", "-L", "--accept-flake-config"]
				inputs: list.Concat([_baseInputs, ["flake.nix", "flake.lock"]])
				outputs: ["result/bin/cuenv"]
			}
		}

		// --- Publishing ---
		publish: {
			type: "group"

			github: schema.#Task & {
				// Stage CI-downloaded binaries and explicitly pass release context.
				hermetic: {passthrough: ["PATH"]}
				env: {
					TAG:      schema.#EnvPassthrough & {name: "GITHUB_REF_NAME"}
					GH_TOKEN: schema.#EnvPassthrough & {name: "GITHUB_TOKEN"}
				}
				inputs: ["dist/*/cuenv"]
				command: "bash"
				args: ["-c", """
					set -euo pipefail
					for dir in dist/*/; do
						platform=$(basename "$dir")
						cp "$dir/cuenv" "dist/cuenv-$platform"
					done
					gh release upload "$TAG" -R cuenv/cuenv dist/cuenv-*
					"""]
				outputs: ["dist/cuenv-*"]
			}

			cue: schema.#Task & {
				// CUE's git source publishes the complete tracked module tree.
				// Publishing uses the caller's CUE auth configuration under HOME.
				hermetic: false
				env: TAG: schema.#EnvPassthrough & {name: "GITHUB_REF_NAME"}
				command: "bash"
				args: ["-c", """
					set -euo pipefail
					task_root="$PWD"
					source_root="$task_root/.cuenv-source"
					mkdir -p .cuenv-cache .cuenv-tmp .git/refs .git/objects "$source_root"
					trap 'rm -rf "$task_root/.cuenv-cache" "$task_root/.cuenv-tmp" "$source_root"' EXIT
					export XDG_CACHE_HOME="$PWD/.cuenv-cache"
					export CUE_CACHE_DIR="$PWD/.cuenv-cache/cue"
					export TMPDIR="$PWD/.cuenv-tmp"
					export TMP="$TMPDIR"
					export TEMP="$TMPDIR"
					export GIT_CONFIG_NOSYSTEM=1
					export GIT_CONFIG_GLOBAL=/dev/null

					TAG=${TAG:-$(git describe --tags --abbrev=0 2>/dev/null || echo "")}
					if [ -z "$TAG" ]; then
						echo "Error: No git tag found"
						exit 1
					fi

					# Recreate the complete checked-out commit, including tracked
					# symlinks and contrib packages, then give CUE its clean tree.
					git archive --format=tar HEAD | tar -x -C "$source_root"
					git -C "$source_root" init --quiet
					git -C "$source_root" add --force -A
					git -C "$source_root" -c user.name=cuenv -c user.email=cuenv@localhost commit --quiet -m "CUE release source"
					cd "$source_root"
					cue mod publish "v$TAG"
					"""]
				inputs: [
					".git/HEAD",
					".git/objects/**",
					".git/refs/**",
					".git/packed-refs*",
				]
			}

			homebrew: schema.#Task & {
				// Publishing token and tag are explicit task environment inputs.
				hermetic: {passthrough: ["PATH"]}
				dependsOn: [_t.publish.github]
				env: {
					TAG:      schema.#EnvPassthrough & {name: "GITHUB_REF_NAME"}
					GH_TOKEN: schema.#OnePasswordRef & {ref: "op://cuenv-github/homebrew-tap/password"}
				}
				command: "bash"
				args: ["-c", """
					set -euo pipefail

					TAG=${TAG:-$(git describe --tags --abbrev=0 2>/dev/null || echo "")}
					if [ -z "$TAG" ]; then
						echo "Error: No git tag found"
						exit 1
					fi

					REPO="cuenv/cuenv"
					TAP_REPO="cuenv/homebrew-tap"
					DARWIN_ARM64_URL="https://github.com/${REPO}/releases/download/${TAG}/cuenv-darwin-arm64"
					LINUX_X64_URL="https://github.com/${REPO}/releases/download/${TAG}/cuenv-linux-x64"
					LINUX_ARM64_URL="https://github.com/${REPO}/releases/download/${TAG}/cuenv-linux-arm64"

					# Download and checksum
					mkdir -p .cuenv-tmp
					TMPDIR="$PWD/.cuenv-tmp"
					export TMPDIR
					task_tmpdir=$(mktemp -d)
					trap 'rm -rf "$task_tmpdir"' EXIT

					gh release download "$TAG" -R "$REPO" -p "cuenv-darwin-arm64" -D "$task_tmpdir"
					gh release download "$TAG" -R "$REPO" -p "cuenv-linux-x64" -D "$task_tmpdir"
					gh release download "$TAG" -R "$REPO" -p "cuenv-linux-arm64" -D "$task_tmpdir"

					DARWIN_SHA256=$(shasum -a 256 "$task_tmpdir/cuenv-darwin-arm64" | awk '{print $1}')
					LINUX_X64_SHA256=$(shasum -a 256 "$task_tmpdir/cuenv-linux-x64" | awk '{print $1}')
					LINUX_ARM64_SHA256=$(shasum -a 256 "$task_tmpdir/cuenv-linux-arm64" | awk '{print $1}')

					# Generate formula
					FORMULA=$(cat <<RUBY
					class Cuenv < Formula
					  desc "Modern application build toolchain with typed environments and CUE-powered task orchestration"
					  homepage "https://github.com/cuenv/cuenv"
					  version "${TAG}"
					  license "AGPL-3.0-or-later"

					  on_macos do
					    on_arm do
					      url "${DARWIN_ARM64_URL}"
					      sha256 "${DARWIN_SHA256}"
					    end
					  end

					  on_linux do
					    on_intel do
					      url "${LINUX_X64_URL}"
					      sha256 "${LINUX_X64_SHA256}"
					    end

					    on_arm do
					      url "${LINUX_ARM64_URL}"
					      sha256 "${LINUX_ARM64_SHA256}"
					    end
					  end

					  def install
					    binary = if OS.mac? && Hardware::CPU.arm?
					      "cuenv-darwin-arm64"
					    elsif OS.linux? && Hardware::CPU.intel?
					      "cuenv-linux-x64"
					    elsif OS.linux? && Hardware::CPU.arm?
					      "cuenv-linux-arm64"
					    else
					      odie "Unsupported platform"
					    end
					    bin.install binary => "cuenv"
					  end

					  test do
					    assert_match version.to_s, shell_output("#{bin}/cuenv --version")
					  end
					end
					RUBY
					)

					# Push to tap repo
					ENCODED=$(printf '%s' "$FORMULA" | base64 | tr -d '\n')
					EXISTING_SHA=$(gh api "repos/${TAP_REPO}/contents/Formula/cuenv.rb" --jq '.sha' 2>/dev/null || echo "")

					if [ -n "$EXISTING_SHA" ]; then
						gh api -X PUT "repos/${TAP_REPO}/contents/Formula/cuenv.rb" -f message="bump: cuenv ${TAG}" -f content="$ENCODED" -f sha="$EXISTING_SHA" -f branch="main"
					else
						gh api -X PUT "repos/${TAP_REPO}/contents/Formula/cuenv.rb" -f message="bump: cuenv ${TAG}" -f content="$ENCODED" -f branch="main"
					fi

					echo "Homebrew formula updated to ${TAG}"
					"""]
			}
		}

		// --- Experimental (LLM Evaluation) ---
		eval: {
			type: "group"

			let _inputs = ["llms.txt", "schema/**", "prompts/**"]

			"task-gen": schema.#Task & {
				command: "gh"
				args: ["models", "eval", "prompts/cuenv-task-generation.prompt.yml"]
				inputs: _inputs
			}

			"task-script": schema.#Task & {
				command: "gh"
				args: ["models", "eval", "prompts/cuenv-task-script-generation.prompt.yml"]
				inputs: _inputs
			}

			"task-script-embed": schema.#Task & {
				command: "gh"
				args: ["models", "eval", "prompts/cuenv-task-script-embed.prompt.yml"]
				inputs: _inputs
			}

			"env-gen": schema.#Task & {
				command: "gh"
				args: ["models", "eval", "prompts/cuenv-env-generation.prompt.yml"]
				inputs: _inputs
			}

			qa: schema.#Task & {
				command: "gh"
				args: ["models", "eval", "prompts/cuenv-question-answering.prompt.yml"]
				inputs: _inputs
			}
		}
	}
}
