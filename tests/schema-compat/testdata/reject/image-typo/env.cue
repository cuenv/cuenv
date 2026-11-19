// want: contxt: field not allowed
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
