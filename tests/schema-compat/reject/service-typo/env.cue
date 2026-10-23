// want: restartt: field not allowed
package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "reject"

services: web: {
	entrypoint: {command: "python3", args: ["-m", "http.server"]}
	restartt: "always"
}
