package cuenv

import (
	"list"
	"github.com/cuenv/cuenv/schema"
	xBun "github.com/cuenv/cuenv/contrib/bun"
	xRust "github.com/cuenv/cuenv/contrib/rust"
	xTools "github.com/cuenv/cuenv/contrib/tools"
)

schema.#Project & {
	name: "cuenv"

	runtime: schema.#ToolsRuntime & {
		platforms: ["darwin-arm64", "darwin-x86_64", "linux-x86_64", "linux-arm64"]
		flakes: {
			nixpkgs: "github:NixOS/nixpkgs/nixos-unstable"
		}
		tools: {
			jq: xTools.#Jq & {version: "1.7.1"}
			yq: xTools.#Yq & {version: "4.44.6"}
			treefmt: xTools.#Treefmt & {version: "2.4.0"}
			bun: xBun.#Bun & {version: "1.3.5"}

			prettier: schema.#Tool & {
				version: "3.7.4"
				source: schema.#Nix & {
					flake:   "nixpkgs"
					package: "nodePackages.prettier"
				}
			}

			// Rust toolchain
			rust: xRust.#Rust & {version: "1.92.0"}
			"rust-analyzer": xRust.#RustAnalyzer & {version: "2025-12-22"}

			// Cargo extensions
			"cargo-nextest": xRust.#CargoNextest & {version: "0.9.116"}
			"cargo-deny": xRust.#CargoDeny & {version: "0.18.9"}
			"cargo-llvm-cov": xRust.#CargoLlvmCov & {version: "0.6.21"}
			"cargo-cyclonedx": xRust.#CargoCyclonedx & {version: "0.5.7"}
			"cargo-zigbuild": xRust.#CargoZigbuild & {version: "0.20.1"}
			sccache: xRust.#SccacheTool & {version: "0.12.0"}

			// Build tools
			zig: xRust.#Zig & {version: "0.15.2"}

			"nixpkgs-fmt": schema.#Tool & {
				version: "nixos-unstable"
				source: schema.#Nix & {
					flake:   "nixpkgs"
					package: "nixpkgs-fmt"
				}
			}
		}
	}

	hooks: onEnter: tools: schema.#ToolsActivate

	// Build cuenv from source instead of using released binaries
	// We really need to find a way to speed this up later.
	config: ci: cuenv: {source: "git", version: "self"}

	env: {
		CLOUDFLARE_ACCOUNT_ID: "340c8fced324c509d19e79ada8f049db"

		environment: production: {
			CACHIX_AUTH_TOKEN: schema.#OnePasswordRef & {ref: "op://cuenv-github/cachix/password"}
			CLOUDFLARE_API_TOKEN: schema.#OnePasswordRef & {ref: "op://cuenv-github/cloudflare/password"}
			CODECOV_TOKEN: schema.#OnePasswordRef & {ref: "op://cuenv-github/codecov/password"}
			CUE_REGISTRY_TOKEN: schema.#OnePasswordRef & {ref: "op://cuenv-github/cue/password"}
			VSCE_PAT: schema.#OnePasswordRef & {ref: "op://cuenv-github/visual-studio-code/password"}
		}
	}

	ci: {
		contributors: sccache: xRust.#Sccache

		provider: github: {
			runner: "blacksmith-8vcpu-ubuntu-2404"
			runners: {
				arch: {
					"linux-x64":    "namespace-profile-cuenv-linux-x86"
					"linux-arm64":  "namespace-profile-cuenv-linux-arm64"
					"darwin-arm64": "namespace-profile-cuenv-macos-arm64"
				}
			}

			cachix: {
				name: "cuenv"
			}

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

		// TODO: This could be a map
		pipelines: [
			{
				name: "sync-check"
				when: {
					branch:      "main"
					pullRequest: true
				}
				provider: github: permissions: {
					"id-token": "write"
				}
				tasks: ["ci.sync-check"]
			},
			{
				name: "ci"
				when: {
					branch:      "main"
					pullRequest: true
				}
				provider: github: permissions: {
					"id-token": "write"
				}
				tasks: ["check"]
			},
			{
				name:        "release"
				environment: "production"
				when: {
					release: ["published"]
					manual: {
						tag_name: {
							description: "Tag to release (e.g., v0.16.0)"
							required:    true
							type:        "string"
						}
					}
				}
				provider: github: permissions: {
					contents:   "write"
					"id-token": "write"
				}
				tasks: [
					{
						task: "nix.build"
						matrix: {
							arch: ["linux-x64", "darwin-arm64"]
						}
					},
					{
						task: "publish"
						artifacts: [{
							from:   "nix.build"
							to:     "dist"
							filter: "" // All variants (default)
						}]
						params: {
							tag:   "${{ github.ref_name }}"
							paths: "dist/**/*"
						}
					},
					"docs.deploy",
				]
			},
		]
	}

	tasks: {
		_baseInputs: [
			"Cargo.toml",
			"Cargo.lock",
			"crates/**",
		]

		ci: {
			"sync-check": {
				command: "cuenv"
				args: ["sync", "ci", "--check"]
				description: "Verify CI workflows are in sync with CUE configuration"
				inputs: ["env.cue", "schema/**", "cue.mod/**", "contrib/**"]
			}
		}

		check: {
			script: """
				set -e
				echo "Running clippy..."
				cargo clippy --workspace --all-targets --all-features -- -D warnings
				echo "Running tests..."
				cargo test --workspace --all-features
				echo "Running security checks..."
				cargo deny check bans licenses advisories
				echo "All checks passed!"
				"""
			inputs: list.Concat([_baseInputs, ["deny.toml", "treefmt.toml", "_tests/**", "features/**", "examples/**", "schema/**", "cue.mod/**"]])
		}

		// schema.#Rust.#Lint?
		lint: {
			command: "cargo"
			args: ["clippy", "--workspace", "--all-targets", "--all-features", "--", "-D", "warnings"]
			inputs: _baseInputs
		}

		fmt: {
			_inputs: [
				".config/**",
				".gitignore",
				".release-please-manifest.json",
				"AGENTS.md",
				"bun.lock",
				"Cargo.lock",
				"Cargo.toml",
				"crates/**",
				"cue.mod/**",
				"deny.toml",
				"docs/**",
				"env.cue",
				"examples/**",
				"features/**",
				"flake.lock",
				"flake.nix",
				"Formula/**",
				"GEMINI.md",
				"HOMEBREW_TAP.md",
				"license.md",
				"package.json",
				"readme.md",
				"release-please-config.json",
				"release.toml",
				"schema/**",
				"_tests/**",
				"treefmt.toml",
			]
			fix: {
				command: "treefmt"
				inputs:  _inputs
			}
			check: {
				command: "treefmt"
				args: ["--fail-on-change"]
				inputs: _inputs
			}
		}

		tests: {
			unit: {
				command: "cargo"
				args: ["nextest", "run", "--workspace", "--all-features"]
				inputs: list.Concat([_baseInputs, ["_tests/**", "features/**", "examples/**", "schema/**", "cue.mod/**"]])
			}
			doc: {
				command: "cargo"
				args: ["test", "--doc", "--workspace"]
				inputs: _baseInputs
			}
			bdd: {
				command: "cargo"
				args: ["test", "--test", "bdd"]
				inputs: list.Concat([_baseInputs, ["_tests/**", "features/**", "schema/**", "cue.mod/**"]])
				outputs: [".test"]
			}
		}

		build: {
			command: "cargo"
			args: ["build", "--workspace", "--all-features"]
			inputs: _baseInputs
		}

		cross: {
			linux: {
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

		security: {
			deny: {
				command: "cargo"
				args: ["deny", "check", "bans", "licenses", "advisories"]
				inputs: list.Concat([_baseInputs, ["deny.toml"]])
			}
		}

		sbom: {
			command: "cargo"
			args: ["cyclonedx", "--override-filename", "sbom.json"]
			inputs: _baseInputs
			outputs: ["sbom.json"]
		}

		coverage: {
			command: "cargo"
			args: ["llvm-cov", "nextest", "--workspace", "--all-features", "--lcov", "--output-path", "lcov.info"]
			inputs: list.Concat([_baseInputs, ["_tests/**", "features/**", "examples/**", "schema/**", "cue.mod/**"]])
			outputs: ["lcov.info"]

		}

		bench: {
			command: "cargo"
			args: ["bench", "--workspace", "--no-fail-fast"]
			inputs: _baseInputs
		}

		docs: {
			build: {
				command: "bash"
				args: ["-c", "bun install --frozen-lockfile && cd docs && bun run build"]
				inputs: [
					"package.json",
					"bun.lock",
					"docs/**",
				]
				outputs: ["docs/dist"]
			}
			deploy: {
				command: "bash"
				args: ["-c", "cd docs && npx wrangler deploy"]
				dependsOn: ["docs.build"]
				inputs: [{task: "docs.build"}]
			}
		}

		eval: {
			_inputs: ["llms.txt", "schema/**", "prompts/**"]

			"task-gen": {
				command: "gh"
				args: ["models", "eval", "prompts/cuenv-task-generation.prompt.yml"]
				inputs: _inputs
			}

			"env-gen": {
				command: "gh"
				args: ["models", "eval", "prompts/cuenv-env-generation.prompt.yml"]
				inputs: _inputs
			}

			qa: {
				command: "gh"
				args: ["models", "eval", "prompts/cuenv-question-answering.prompt.yml"]
				inputs: _inputs
			}
		}

		nix: {
			build: {
				command: "nix"
				args: ["build", ".#cuenv"]
				inputs: _baseInputs
				outputs: ["result/bin/cuenv"]
			}
		}

		publish: github: {
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

		publish: cue: {
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

		cargo: install: {
			command: "cargo"
			args: ["install", "--path", "./crates/cuenv"]
			inputs: _baseInputs
		}
	}
}
