// want: env."1FOO": field not allowed
package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "reject"

env: "1FOO": "value"
