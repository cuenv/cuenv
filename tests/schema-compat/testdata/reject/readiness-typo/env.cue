// want: intervall: field not allowed
package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "reject"

services: web: {
	entrypoint: {command: "python3", args: ["-m", "http.server"]}
	readiness: {kind: "port", port: 8000, intervall: "1s"}
}
