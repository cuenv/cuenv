// want: restartt: field not allowed
// min-language: v0.18.0
// Before min-language a top-level typo in a services entry written without
// schema.#Service & is accepted. The compat check requires that.
package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "reject"

services: web: {
	// #Command keeps the entrypoint concrete. The service entry itself stays
	// untyped, which is the gap: restartt is ignored before language v0.18.
	entrypoint: schema.#Command & {
		command: "python3"
		args: ["-m", "http.server"]
	}
	restartt: "always"
}
