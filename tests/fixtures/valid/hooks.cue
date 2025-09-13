package cuenv

import "github.com/cuenv/cuenv/schema"

// Test various hook configurations
schema.#Cuenv & {
	hooks: {
		// Single hook as object
		onEnter: {
			command: "source"
			args: [".env"]
			source: true
			dir: "."
			inputs: [".env", "config.yaml"]
		}
		
		// Multiple hooks as array
		onExit: [
			{
				command: "cleanup"
				args: ["--force"]
			},
			{
				command: "notify"
				args: ["Environment deactivated"]
			}
		]
	}
}