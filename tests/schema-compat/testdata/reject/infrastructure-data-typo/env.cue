// want: configurations: field not allowed
package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "reject"

infrastructure: {
	state: turso: url: "http://127.0.0.1:8080"
	providers: fake: {source: "example/fake", version: "1.0.0"}
	data: lookup: {type: "fake_lookup", configurations: {}}
}
