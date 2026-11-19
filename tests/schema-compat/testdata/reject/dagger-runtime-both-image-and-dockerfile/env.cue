// want: explicit error (_|_ literal) in source
package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "reject"

runtime: schema.#DaggerRuntime & {
	image:      "rust:1.100-slim"
	dockerfile: "Dockerfile"
}
