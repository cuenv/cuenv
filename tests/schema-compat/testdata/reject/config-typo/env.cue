// want: outputFormatt: field not allowed
package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "reject"

config: outputFormatt: "tui"
