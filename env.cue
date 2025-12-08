package cuenv

import (
	"list"
	"github.com/cuenv/cuenv/schema"
)

schema.#Cuenv

env: NAME: "David 2"
env: WHO: "rawkode"

ci: pipelines: [
	{
		name: "ci"
		tasks: ["check"]
	},
	{
		name: "release"
		tasks: [
			"release.publish-cue",
			"release.publish-crates",
			"docs.deploy",
		]
	},
	{
		name: "release-pr"
		tasks: ["release.generate-pr"]
	},
]

hooks: {
	onEnter: {
		nix: schema.#NixFlake
	}
}

tasks: {
	// Common inputs for Rust/Cargo tasks
	#BaseInputs: [
		"Cargo.toml",
		"Cargo.lock",
		"crates",
	]

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
			inputs: _inputs
		}
		default: fix
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

	schema: {
		generate: {
			command: "cargo"
			args: [
				"run",
				"--package",
				"schema-validator",
				"--",
				"generate",
				"--output",
				"generated-schemas",
			]
			inputs: list.Concat([#BaseInputs, ["schema", "tests/fixtures"]])
			outputs: ["generated-schemas"]
		}

		export: {
			command: "bash"
			args: [
				"-c",
				"cue export schema/*.cue > cue-schemas.json || true; cue export --out openapi schema/*.cue > cue-openapi.json || true",
			]
			inputs: ["schema", "cue.mod"]
			outputs: [
				"cue-schemas.json",
				"cue-openapi.json",
			]
		}

		validate: [
			{
				command: "cargo"
				args: ["test", "--package", "cuenv-core", "schema_conformance"]
				inputs: list.Concat([#BaseInputs, ["schema", "tests/fixtures"]])
			},
			{
				command: "cargo"
				args: [
					"run",
					"--package",
					"schema-validator",
					"--",
					"validate",
					"--fixtures",
					"tests/fixtures",
				]
				inputs: list.Concat([#BaseInputs, ["schema", "tests/fixtures", "cue.mod"]])
			},
		]

		compare: {
			command: "cargo"
			args: [
				"run",
				"--package",
				"schema-validator",
				"--",
				"compare",
				"--cue-path",
				"schema",
				"--rust-path",
				"generated-schemas",
			]
			dependsOn: ["generate"]
			inputs: list.Concat([#BaseInputs, ["schema", "generated-schemas", "cue.mod"]])
		}

		ci: [
			generate,
			export,
			validate,
			compare,
		]
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
			command: "cue"
			args: ["mod", "publish", "$TAG"]
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
