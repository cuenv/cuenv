// want: subdirr: field not allowed
package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "reject"

vcs: dep: {
	url:     "https://example.com/r.git"
	vendor:  true
	path:    "vendor/r"
	subdirr: "lib"
}
