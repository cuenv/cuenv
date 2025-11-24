package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Cuenv

hooks: onEnter: {
	schema.#NixFlake
}

tasks: {
	// Common inputs for Rust/Cargo tasks
	_cargoInputs: [
		"Cargo.toml",
		"Cargo.lock",
		"crates",
	]

	lint: {
		command: "cargo"
		args: ["clippy", "--workspace", "--all-targets", "--all-features", "--", "-D", "warnings"]
		inputs: _cargoInputs
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
			inputs: _cargoInputs + ["tests", "features"]
		}
		doc: {
			command: "cargo"
			args: ["test", "--doc", "--workspace"]
			inputs: _cargoInputs
		}
		bdd: {
			command: "cargo"
			args: ["test", "--test", "bdd"]
			inputs: _cargoInputs + ["tests", "features"]
		}
	}

	build: {
		command: "cargo"
		args: ["build", "--workspace", "--all-features"]
		inputs: _cargoInputs
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
			inputs: _cargoInputs + ["deny.toml"]
		}
	}

	sbom: {
		command: "cargo"
		args: ["cyclonedx", "--override-filename", "sbom.json"]
		inputs: _cargoInputs
		outputs: ["sbom.json"]
	}

	coverage: {
		command: "cargo"
		args: ["llvm-cov", "--workspace", "--all-features", "--lcov", "--output-path", "lcov.info"]
		inputs: _cargoInputs
		outputs: ["lcov.info"]
	}

	bench: {
		command: "cargo"
		args: ["bench", "--workspace", "--no-fail-fast"]
		inputs: _cargoInputs
	}
}
