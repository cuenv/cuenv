// want: incomplete value
package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "reject"

// A filter is data. A CUE constraint is not, and `cue export` refuses it.
contexts: deploy: schema.#TaskFilter & {name: =~"^deploy"}
