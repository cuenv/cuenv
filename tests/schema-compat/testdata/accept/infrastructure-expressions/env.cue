package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "output-expressions"

// A template in a hidden field carries references like any other value.
_sink: {
	bucket: infrastructure.resources.bucket.outputs.name
	token:  infrastructure.resources.token.outputs.value
}

infrastructure: {
	state: turso: url: "http://127.0.0.1:8080"
	providers: fake: {source: "example/fake", version: "1.0.0"}
	resources: {
		bucket: {type: "fake_bucket", outputs: {name: type: "string", size: type: "number", public: type: "bool", tags: type: "list"}}
		token:  {type: "fake_token", outputs: {id: type: "string", value: type: "string"}}
		consumer: {
			type: "fake_object"
			configuration: {
				direct:  infrastructure.resources.token.outputs.id
				size:    infrastructure.resources.bucket.outputs.size
				public:  infrastructure.resources.bucket.outputs.public
				sink:    _sink
				listed:  [infrastructure.resources.token.outputs.id, "literal"]
				name:    schema.#Format & {parts: ["app-", infrastructure.resources.bucket.outputs.name, "-", infrastructure.resources.bucket.outputs.size]}
				secret:  schema.#Sha256 & {of: infrastructure.resources.token.outputs.value}
				encoded: schema.#Base64 & {of: schema.#Format & {parts: [infrastructure.resources.token.outputs.id, ":", infrastructure.resources.token.outputs.value]}, encoding: "url"}
				joined:  schema.#Join & {items: [infrastructure.resources.bucket.outputs.name, "static"], separator: ","}
				first:   schema.#Select & {from: infrastructure.resources.bucket.outputs.tags, path: [0], type: "string"}
			}
		}
	}
	environments: production: {
		providers: fake: {source: "example/fake", version: "1.0.0"}
		resources: {
			bucket: {type: "fake_bucket", outputs: name: type: "string"}
			consumer: {
				type: "fake_object"
				configuration: value: infrastructure.environments.production.resources.bucket.outputs.name
			}
		}
	}
}
