package _tests

import "github.com/cuenv/cuenv/schema"

// Test various hook configurations
schema.#Project & {
	name: "hooks-test"
	hooks: {
		// Hooks are maps with string keys
		onEnter: {
			"source-env": {
				command: "source"
				args: [".env"]
				source: true
				dir:    "."
				inputs: [".env", "config.yaml"]
			}
		}

		// Multiple hooks as named entries in the map
		onExit: {
			"cleanup": {
				command: "cleanup"
				args: ["--force"]
			}
			"notify": {
				command: "notify"
				args: ["Environment deactivated"]
			}
		}

		// Pre-push hooks run before git push, filtered by changed files.
		// The following environment variables are available to hook commands:
		//   CUENV_CHANGED_FILES      - Newline-separated list of files changed in the push
		//   CUENV_PRE_PUSH_LOCAL_SHA - SHA of the local commit being pushed
		//   CUENV_PRE_PUSH_REMOTE_SHA - SHA of the remote commit (0000... for new branches)
		prePush: {
			"lint": {
				command: "cuenv"
				args: ["task", "lint"]
				// Only run if source files changed
				inputs: ["src/**/*.rs", "crates/**/*.rs"]
			}
			"test": {
				command: "cuenv"
				args: ["task", "test.unit"]
				// Run tests if any Rust files or Cargo.toml changed
				inputs: ["**/*.rs", "Cargo.toml", "Cargo.lock"]
				order:  200 // Run after lint
			}
			"format-check": {
				command: "cuenv"
				args: ["task", "fmt.check"]
				// Check formatting for any changed files
				inputs: ["**/*.rs", "**/*.go", "**/*.cue"]
				order:  50 // Run first
			}
		}
	}
}