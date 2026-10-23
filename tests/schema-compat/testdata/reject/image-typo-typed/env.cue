// want: contxt: field not allowed
package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "reject"

images: api: schema.#ContainerImage & {
	context: "."
	contxt:  "."
}
