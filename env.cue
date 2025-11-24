package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Cuenv

hooks: onEnter: {
	schema.#NixFlake
}

tasks: {
	lint: {
		command: "cargo"
		args: ["clippy", "--workspace", "--all-targets", "--all-features", "--", "-D", "warnings"]
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
		}
		doc: {
			command: "cargo"
			args: ["test", "--doc", "--workspace"]
		}
		bdd: {
			command: "cargo"
			args: ["test", "--test", "bdd"]
		}
	}

	build: {
		command: "cargo"
		args: ["build", "--workspace", "--all-features"]
	}

	security: {
		audit: {
			command: "cargo"
			args: ["audit"]
		}
		deny: {
			command: "cargo"
			args: ["deny", "check", "bans", "licenses", "advisories"]
		}
	}

	sbom: {
		command: "cargo"
		args: ["cyclonedx", "--override-filename", "sbom.json"]
	}

	coverage: {
		command: "cargo"
		args: ["llvm-cov", "--workspace", "--all-features", "--lcov", "--output-path", "lcov.info"]
	}

	bench: {
		command: "cargo"
		args: ["bench", "--workspace", "--no-fail-fast"]
	}
}
