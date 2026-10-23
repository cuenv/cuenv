// want: environmnt: field not allowed
package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "reject"

env: {
	FOO: "x"
	environmnt: production: BAR: "y"
}
