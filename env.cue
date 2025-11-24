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
		fix: {
			command: "treefmt"
		}
		check: {
			command: "treefmt"
			args: ["--fail-on-change"]
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
