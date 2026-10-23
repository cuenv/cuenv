// want: resource: field not allowed
package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "reject"

infrastructure: {
	state: turso: url: "http://127.0.0.1:8080"
	resource: {}
}
