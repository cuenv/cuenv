// want: restartt: field not allowed
package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "reject"

services: web: {
	// #Command keeps the entrypoint concrete. The surrounding service
	// gets its type from the project schema.
	entrypoint: schema.#Command & {
		command: "python3"
		args: ["-m", "http.server"]
	}
	restartt: "always"
}
