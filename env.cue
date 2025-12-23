package cuenv

import (
	"list"
	"github.com/cuenv/cuenv/schema"
)

schema.#Project & {
	name: "cuenv"

	// runtime: nix should provide hooks?
	runtime: schema.#NixRuntime
	hooks: onEnter: nix: schema.#NixFlake

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

	owners: rules: default: {
		pattern: "**"
		owners: ["@rawkode"]
	}

	ignore: {
		git: [
			".cache",
			".cargo",
			".cuenv",
			".test",
			"*.vsix",
			"bdd_test_runs",
			"crates/cuengine/vendor",
			"dist",
			"node_modules",
			"result",
			"target",
		]
	}

	ci: {
		provider: github: {
			runner: "blacksmith-8vcpu-ubuntu-2404"
			runners: {
				arch: {
					"linux-x64":    "blacksmith-8vcpu-ubuntu-2404"
					"linux-arm64":  "blacksmith-8vcpu-ubuntu-2404-arm"
					"darwin-arm64": "macos-14"
				}
			}

			cachix: {
				name:       "cuenv"
				pushFilter: "(-source$|nixpkgs\\.tar\\.gz$)"
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
				tasks: ["ci.sync-check"]
			},
			{
				name: "ci"
				when: {
					branch:      "main"
					pullRequest: true
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
							arch: ["linux-x64", "linux-arm64", "darwin-arm64"]
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
			"crates",
		]

		// There should be a Cuenv way to trigger cuenv checks?
		// schema.#CuenvCommand & { command: "sync --check" } ?
		// ci: syncCheck: {
		// 	command: "./result/bin/cuenv"
		// 	args: ["sync", "ci", "--check"]
		// 	description: "Verify CI workflows are in sync with CUE configuration"
		// 	inputs: ["env.cue", "schema", "cue.mod"]
		// }

		// Shared Modules should provide: schema.#NixFlakeCheck
		check: {
			command: "nix"
			args: ["flake", "check"]
			inputs: [
				"flake.nix",
				"flake.lock",
				"Cargo.toml",
				"Cargo.lock",
				"crates",
				"schema",
				"cue.mod",
				"deny.toml",
			]
		}

		// schema.#Rust.#Lint?
		lint: {
			command: "cargo"
			args: ["clippy", "--workspace", "--all-targets", "--all-features", "--", "-D", "warnings"]
			inputs: _baseInputs
		}

		fmt: {
			_inputs: [
				".config",
				".gitignore",
				".release-please-manifest.json",
				"AGENTS.md",
				"bun.lock",
				"Cargo.lock",
				"Cargo.toml",
				"crates",
				"cue.mod",
				"deny.toml",
				"docs",
				"env.cue",
				"examples",
				"features",
				"flake.lock",
				"flake.nix",
				"Formula",
				"GEMINI.md",
				"HOMEBREW_TAP.md",
				"license.md",
				"package.json",
				"readme.md",
				"release-please-config.json",
				"release.toml",
				"schema",
				"tests",
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
				inputs: list.Concat([_baseInputs, ["tests", "features", "examples", "schema", "cue.mod"]])
			}
			doc: {
				command: "cargo"
				args: ["test", "--doc", "--workspace"]
				inputs: _baseInputs
			}
			bdd: {
				command: "cargo"
				args: ["test", "--test", "bdd"]
				inputs: list.Concat([_baseInputs, ["tests", "features", "schema", "cue.mod"]])
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
			inputs: list.Concat([_baseInputs, ["tests", "features", "examples", "schema", "cue.mod"]])
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
					"docs",
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
			_inputs: ["llms.txt", "schema", "prompts"]

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
			command: "gh"
			args: ["release", "upload", "{{tag}}", "{{paths}}"]
			params: {
				tag: {
					description: "Git tag to upload to"
					required:    true
				}
				paths: {
					description: "Glob pattern for files to upload"
					required:    true
				}
			}
		}

		publish: crates: {
			command: "cargo"
			args: ["publish", "-p", "cuenv"]
			dependsOn: ["publish.cue"]
		}

		publish: cue: {
			command: "bash"
			args: ["-c", """
				TAG=$(git describe --tags --abbrev=0 2>/dev/null || echo "")
				if [ -z "$TAG" ]; then
					echo "Error: No git tag found"
					exit 1
				fi
				cue login --token=$CUE_REGISTRY_TOKEN && cue mod publish $TAG
				"""]
			inputs: ["cue.mod", "schema"]
		}
	}
}
