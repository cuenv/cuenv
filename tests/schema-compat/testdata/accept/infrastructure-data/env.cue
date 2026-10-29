package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "data-sources"

infrastructure: {
	state: turso: url: "http://127.0.0.1:8080"
	providers: fake: {source: "example/fake", version: "1.0.0"}
	data: lookup: {type: "fake_lookup", configuration: query: "name"}
	resources: consumer: {
		type: "fake_object"
		configuration: value: schema.#InfrastructureOutputReference & {
			fromDataSource: "fake_lookup.lookup"
			attribute:      "id"
		}
	}
}
