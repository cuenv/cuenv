package cuenv

import (
	"list"
	"github.com/cuenv/cuenv/schema"
)

schema.#Project

name: "cuenv"

hooks: onEnter: nix: schema.#NixFlake

env: {
    CLOUDFLARE_ACCOUNT_ID: "340c8fced324c509d19e79ada8f049db"

    environment: production: {
        CACHIX_AUTH_TOKEN: "op://cuenv-github/cachix/password"
        CLOUDFLARE_API_TOKEN: "op://cuenv-github/cloudflare/password"
        CODECOV_TOKEN: "op://cuenv-github/codcov/password"
        CUE_REGISTRY_TOKEN: "op://cuenv-github/cue/password"
        VSCE_PAT: "op://cuenv-github/visual-studio-code/password"
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

	pipelines: [
		// Sync check pipeline - verifies generated workflows are in sync
		{
			name: "sync-check"
			when: {
				branch:      "main"
				pullRequest: true
			}
			tasks: ["ci.sync-check"]
		},
		// CI pipeline - runs on PRs and main branch pushes
		{
			name: "ci"
			when: {
				branch:      "main"
				pullRequest: true
			}
			tasks: ["check"]
		},
		// Release pipeline - runs on GitHub release or manual trigger
		{
			name: "release"
			when: {
				release: ["published"]
				manual: {
					tag_name: {
						description: "Tag to release (e.g., v0.6.0)"
						required:    true
						type:        "string"
					}
				}
			}
			derivePaths: false // Release runs regardless of file changes
			tasks: [
				"release.publish-cue",
				"docs.deploy",
			]
			provider: github: permissions: {
				contents:   "write"
				"id-token": "write"
			}
		},
		// Release PR pipeline - creates release PRs from changesets
		{
			name: "release-pr"
			when: branch: "main"
			tasks: ["release.generate-pr"]
			provider: github: permissions: {
				contents:       "write"
				"pull-requests": "write"
			}
		},
		// Docs deployment - on main push
		{
			name: "deploy"
			when: branch: "main"
			tasks: ["docs.deploy"]
		},
		// LLM evaluation - on changes to prompts/schema, weekly scheduled
		{
			name: "llms-eval"
			when: {
				branch:      "main"
				pullRequest: true
				scheduled:   "0 0 * * 0" // Weekly Sunday midnight UTC
			}
			tasks: ["eval.task-gen", "eval.env-gen", "eval.qa"]
			provider: github: permissions: {
				models: "read"
			}
		},
	]
}

tasks: {
	// Common inputs for Rust/Cargo tasks
	#BaseInputs: [
		"Cargo.toml",
		"Cargo.lock",
		"crates",
	]

	pwd: command: "pwd"

	// CI sync check - verifies generated workflows match committed files
	ci: "sync-check": {
		command: "./result/bin/cuenv"
		args: ["ci", "--format", "github", "--check"]
		description: "Verify CI workflows are in sync with CUE configuration"
		inputs: ["env.cue", "schema", "cue.mod"]
	}

	// CI check task - delegates to nix flake check for optimal caching
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

	lint: {
		command: "cargo"
		args: ["clippy", "--workspace", "--all-targets", "--all-features", "--", "-D", "warnings"]
		inputs: #BaseInputs
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

	test: {
		unit: {
			command: "cargo"
			args: ["nextest", "run", "--workspace", "--all-features"]
			inputs: list.Concat([#BaseInputs, ["tests", "features", "examples", "schema", "cue.mod"]])
		}
		doc: {
			command: "cargo"
			args: ["test", "--doc", "--workspace"]
			inputs: #BaseInputs
		}
		bdd: {
			command: "cargo"
			args: ["test", "--test", "bdd"]
			inputs: list.Concat([#BaseInputs, ["tests", "features", "schema", "cue.mod"]])
			outputs: [".test"]
		}
	}

	build: {
		command: "cargo"
		args: ["build", "--workspace", "--all-features"]
		inputs: #BaseInputs
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
			inputs: #BaseInputs
		}
	}

	security: {
		audit: {
			command: "cargo"
			args: ["audit"]
			inputs: ["Cargo.lock"]
		}
		deny: {
			command: "cargo"
			args: ["deny", "check", "bans", "licenses", "advisories"]
			inputs: list.Concat([#BaseInputs, ["deny.toml"]])
		}
	}

	sbom: {
		command: "cargo"
		args: ["cyclonedx", "--override-filename", "sbom.json"]
		inputs: #BaseInputs
		outputs: ["sbom.json"]
	}

	coverage: {
		command: "cargo"
		args: ["llvm-cov", "nextest", "--workspace", "--all-features", "--lcov", "--output-path", "lcov.info"]
		inputs: list.Concat([#BaseInputs, ["tests", "features", "examples", "schema", "cue.mod"]])
		outputs: ["lcov.info"]
	}

	bench: {
		command: "cargo"
		args: ["bench", "--workspace", "--no-fail-fast"]
		inputs: #BaseInputs
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
			inputsFrom: [{task: "docs.build"}]
		}
	}

	// LLM evaluation tasks using GitHub Models
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

	release: {
		build: {
			command: "cargo"
			args: ["build", "--workspace", "--release"]
			inputs: #BaseInputs
		}

		test: {
			command: "cargo"
			args: [
				"nextest",
				"run",
				"--workspace",
				"--all-features",
				"--release",
			]
			inputs: list.Concat([#BaseInputs, ["tests", "features", "examples", "schema", "cue.mod"]])
		}

		"publish-cue": {
			command: "bash"
			args: ["-c", "cue login --token=$CUE_REGISTRY_TOKEN && cue mod publish $TAG"]
			inputs: ["cue.mod", "schema"]
		}

		"publish-crates": {
			command: "bash"
			args: ["-c", "./result/bin/cuenv release publish"]
			dependsOn: ["release.publish-cue"]
			inputs: #BaseInputs
		}

		"generate-pr": {
			command: "bash"
			args: ["-c", "./result/bin/cuenv changeset from-commits && ./result/bin/cuenv release version"]
			inputs: [".changeset", "Cargo.toml", "crates"]
		}
	}
}
