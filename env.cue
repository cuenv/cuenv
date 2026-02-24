package cuenv

import (
	"list"

	"github.com/cuenv/cuenv/schema"
	xBun "github.com/cuenv/cuenv/contrib/bun"
	xCodecov "github.com/cuenv/cuenv/contrib/codecov"
	xContributors "github.com/cuenv/cuenv/contrib/contributors"
	xRust "github.com/cuenv/cuenv/contrib/rust"
	xTools "github.com/cuenv/cuenv/contrib/tools"
)

// Command template for cargo tasks
#cargo: schema.#Task & {command: "cargo"}

// Shared input patterns for Rust tasks
let _baseInputs = [
	"Cargo.toml",
	"Cargo.lock",
	"crates/**",
]

schema.#Project & {
	name: "cuenv"

	// Alias to avoid scoping conflict with pipeline's tasks field
	let _t = tasks

	// ============================================================================
	// Runtime Configuration
	// ============================================================================

	runtime: schema.#ToolsRuntime & {
		platforms: ["darwin-arm64", "darwin-x86_64", "linux-x86_64", "linux-arm64"]

		flakes: {
			nixpkgs: "github:NixOS/nixpkgs/nixos-unstable"
		}

		tools: {
			// --- General CLI Tools ---
			jq: xTools.#Jq & {version: "1.7.1"}
			yq: xTools.#Yq & {version: "4.44.6"}
			treefmt: xTools.#Treefmt & {version: "2.4.0"}
			cue: xTools.#Cue & {version: "0.15.3"}
			bun: xBun.#Bun & {version: "1.3.5"}

			prettier: schema.#Tool & {
				version: "3.7.4"
				source: schema.#Nix & {
					flake:   "nixpkgs"
					package: "nodePackages.prettier"
				}
			}

			// --- Rust Toolchain ---
			rust: xRust.#Rust & {
				version: "1.92.0"
				source: profile: "default"
				source: components: ["rust-src", "clippy", "rustfmt", "llvm-tools-preview"]
				source: targets: [
					"x86_64-unknown-linux-gnu",
					"aarch64-unknown-linux-gnu",
					"aarch64-apple-darwin",
					"x86_64-apple-darwin",
				]
			}
			"rust-analyzer": xRust.#RustAnalyzer & {version: "2025-12-22"}

			// --- Cargo Extensions ---
			"cargo-nextest": xRust.#CargoNextest & {version: "0.9.116"}
			"cargo-deny": xRust.#CargoDeny & {version: "0.18.9"}
			"cargo-llvm-cov": xRust.#CargoLlvmCov & {version: "0.6.21"}
			"cargo-cyclonedx": xRust.#CargoCyclonedx & {version: "0.5.7"}
			"cargo-zigbuild": xRust.#CargoZigbuild & {version: "0.20.1"}
			sccache: xRust.#SccacheTool & {version: "0.12.0"}

			// --- Build Tools (Nix) ---
			zig: xRust.#Zig & {version: "nixos-unstable"}

			"nixpkgs-fmt": schema.#Tool & {
				version: "nixos-unstable"
				source: schema.#Nix & {
					flake:   "nixpkgs"
					package: "nixpkgs-fmt"
				}
			}
		}
	}

	// ============================================================================
	// Hooks & Formatters
	// ============================================================================

	hooks: onEnter: tools: schema.#ToolsActivate

	formatters: {
		rust: {edition: "2024"}
		go: enabled: true
	}

	// ============================================================================
	// Configuration
	// ============================================================================

	// Build cuenv from source using native Rust/Go toolchains
	// Uses native setup instead of nix to avoid sccache env var issues
	config: ci: cuenv: {source: "native", version: "self"}

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
			xContributors.#CuenvNative,
			xContributors.#OnePassword,
			xRust.#Sccache,
			xCodecov.#Codecov,
		]

		provider: github: {
			runner: "blacksmith-8vcpu-ubuntu-2404"

			runners: arch: {
				"linux-x64":    "namespace-profile-cuenv-linux-x86"
				"linux-arm64":  "namespace-profile-cuenv-linux-arm64"
				"darwin-arm64": "namespace-profile-cuenv-macos-arm64"
			}

			cachix: name: "cuenv"

			pathsIgnore: [
				"docs/**",
				"examples/**",
				"*.md",
				"LICENSE",
				".vscode/**",
			]

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
				when: {
					branch:      "main"
					pullRequest: true
				}
				provider: github: permissions: "id-token": "write"
				tasks: [_t.check]
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
						task: _t.cargo.build
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

		// --- CI Check (runs lint, tests, security) ---
		check: schema.#Task & {
			command: "echo"
			args: ["All checks passed"]
			dependsOn: [lint, tests, security]
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
				args: ["deny", "check", "bans", "licenses", "advisories"]
				inputs: list.Concat([_baseInputs, ["deny.toml"]])
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
				args: ["build", "--release", "-p", "cuenv"]
				inputs: _baseInputs
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
			script: """
					#!/bin/bash
					set -euo pipefail
					TARGET="x86_64-unknown-linux-gnu"

					echo "Building Go bridge for Linux..."
					cd crates/cuengine
					mkdir -p ../../target/release
					export CGO_ENABLED=1 GOOS=linux GOARCH=amd64
					export CC="zig cc -target x86_64-linux-gnu"
					export CXX="zig c++ -target x86_64-linux-gnu"
					export AR="zig ar"
					go build -buildmode=c-archive -o ../../target/release/libcue_bridge.a bridge.go
					cp libcue_bridge.h ../../target/release/
					cd ../..

					echo "Building cuenv for Linux..."
					cargo zigbuild --release --target $TARGET -p cuenv

					echo "Binary at: target/$TARGET/release/cuenv"
					file target/$TARGET/release/cuenv
					"""
				inputs: _baseInputs
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
				command: "bash"
				args: ["-c", """
					TAG=$(git describe --tags --abbrev=0 2>/dev/null || echo "")
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
