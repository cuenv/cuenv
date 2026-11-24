// want: invalid interpolation
package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "reject"

infrastructure: {
	state: turso: url: "http://127.0.0.1:8080"
	providers: fake: {source: "example/fake", version: "1.0.0"}
	resources: {
		bucket: {type: "fake_bucket", outputs: name: type: "string"}
		consumer: {
			type: "fake_object"
			configuration: value: "app-\(infrastructure.resources.bucket.outputs.name)"
		}
	}
}
