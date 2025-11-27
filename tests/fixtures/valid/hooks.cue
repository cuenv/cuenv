package cuenv

import "github.com/cuenv/cuenv/schema"

// Test various hook configurations
schema.#Cuenv & {
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
	}
}