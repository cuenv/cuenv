package cuenv

import "github.com/cuenv/cuenv/schema"

// Invalid: hook missing required 'command' field
schema.#Cuenv & {
	hooks: {
		onEnter: {
			// command is required but missing
			args: ["some", "args"]
			source: true
		}
	}
}