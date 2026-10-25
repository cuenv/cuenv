// want: contxt: field not allowed
// min-language: v0.18.0
// Before min-language a top-level typo in an images entry written without
// schema.#ContainerImage & is accepted. The compat check requires that.
package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "reject"

images: api: {
	context: "."
	contxt:  "."
}
