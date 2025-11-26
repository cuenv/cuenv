package cuenv

import (
	"list"
	"github.com/cuenv/cuenv/schema"
)

schema.#Cuenv

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
			args: ["-c", "cd docs && bun install && bun run build"]
			inputs: [
				"docs",
				"docs/package.json",
				"docs/bun.lock",
				"docs/astro.config.mjs",
				"docs/tsconfig.json",
			]
			outputs: ["docs/dist"]
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
			inputs: ["schema"]
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
				inputs: list.Concat([#BaseInputs, ["schema", "tests/fixtures"]])
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
			inputs: ["schema", "generated-schemas"]
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
	}
}
