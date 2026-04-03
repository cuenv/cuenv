package cuenv

import (
	"list"

	"github.com/cuenv/cuenv/schema"
	xCodecov "github.com/cuenv/cuenv/contrib/codecov"
	xContributors "github.com/cuenv/cuenv/contrib/contributors"
	xNix "github.com/cuenv/cuenv/contrib/nix"
	xRust "github.com/cuenv/cuenv/contrib/rust"
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
	["_tests/**", "contrib/**", "features/**", "examples/**", "schema/**", "cue.mod/**", "deny.toml", "env.cue"],
])

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
			CACHIX_AUTH_TOKEN: schema.#OnePasswordRef & {ref: "op://cuenv-github/cachix/password"}
			CLOUDFLARE_API_TOKEN: schema.#OnePasswordRef & {ref: "op://cuenv-github/cloudflare/password"}
			CODECOV_TOKEN: schema.#OnePasswordRef & {ref: "op://cuenv-github/codecov/password"}
			CUE_REGISTRY_TOKEN: schema.#OnePasswordRef & {ref: "op://cuenv-github/cue/password"}
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
			xContributors.#Cachix,
			xContributors.#CuenvNix,
			xContributors.#OnePassword,
			xRust.#Sccache,
			xCodecov.#Codecov,
		]

		provider: github: {
			runner: "namespace-profile-cuenv-linux-x86"

			runners: arch: {
				"linux-x64":    "namespace-profile-cuenv-linux-x86"
				"linux-arm64":  "namespace-profile-cuenv-linux-arm64"
				"darwin-arm64": "namespace-profile-cuenv-macos-arm64"
			}

			cachix: name: "cuenv"

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
				tasks: [_t.ci."sync-check"]
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
						matrix: arch: ["linux-x64", "darwin-arm64"]
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
				inputs: ["env.cue", "schema/**", "contrib/**"]
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
				inputs: list.Concat([_baseInputs, ["_tests/**", "features/**", "examples/**", "schema/**", "cue.mod/**"]])
			}

			doc: #cargo & {
				args: ["test", "--doc", "--workspace"]
				inputs: _baseInputs
			}

			bdd: #cargo & {
				args: ["test", "--test", "bdd"]
				inputs: list.Concat([_baseInputs, ["_tests/**", "features/**", "schema/**", "cue.mod/**"]])
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
			inputs: list.Concat([_baseInputs, ["_tests/**", "features/**", "examples/**", "schema/**", "cue.mod/**"]])
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
				command: "bash"
				args: ["-c", "bun install --frozen-lockfile && cd docs && bun run build && cp public/.assetsignore dist/"]
				inputs: [
					"package.json",
					"bun.lock",
					"docs/**",
				]
				outputs: ["docs/dist"]
			}

			deploy: schema.#Task & {
				command: "bash"
				args: ["-c", "cd docs && npx wrangler deploy"]
				dependsOn: [_t.docs.build]
				inputs: [{task: "docs.build"}]
			}
		}

		// --- Build & Release ---
		cargo: {
			type: "group"

			compile: #cargo & {
				args: ["build", "--workspace", "--all-targets", "--all-features"]
				inputs: _baseInputs
			}

			build: #cargo & {
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
				command: "bash"
				args: ["-c", """
					for dir in dist/*/; do
						platform=$(basename "$dir")
						mv "$dir/cuenv" "dist/cuenv-$platform"
					done
					rm -rf dist/*/
					gh release upload $TAG dist/cuenv-*
					"""]
			}

			cue: schema.#Task & {
				env: TAG: schema.#EnvPassthrough & {name: "GITHUB_REF_NAME"}
				command: "bash"
				args: ["-c", """
					TAG=${TAG:-$(git describe --tags --abbrev=0 2>/dev/null || echo "")}
					if [ -z "$TAG" ]; then
						echo "Error: No git tag found"
						exit 1
					fi
					cue login --token=$CUE_REGISTRY_TOKEN && cue mod publish v$TAG
					"""]
				inputs: ["cue.mod/**", "schema/**"]
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
