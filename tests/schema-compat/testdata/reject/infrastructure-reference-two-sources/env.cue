// want: empty disjunction
package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "reject"

infrastructure: {
	state: turso: url: "http://127.0.0.1:8080"
	providers: fake: {source: "example/fake", version: "1.0.0"}
	resources: consumer: {
		type: "fake_object"
		configuration: value: schema.#InfrastructureOutputReference & {
			fromResource:   "fake_lookup.primary"
			fromDataSource: "fake_lookup.primary"
			attribute:      "id"
		}
	}
}
