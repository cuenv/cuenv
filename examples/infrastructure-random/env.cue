package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "infrastructure-random"

// Managed resources driven through the hashicorp/random provider plugin.
// State lives in Turso, keyed by this CUE module path and project name.
//
//   sqld --http-listen-addr 127.0.0.1:8080   # or point url at libsql://...
//   cuenv i plan  -p examples/infrastructure-random --package examples
//   cuenv i apply -p examples/infrastructure-random --package examples
//
// This example keeps `configuration` untyped: the provider still validates
// every argument at plan time, and the end-to-end tests swap in a local
// provider `path`. To type it, run
// `cuenv infrastructure provider add hashicorp/random@3.9.1` and use the
// generated `#Provider` and `#Resource` definitions (see the infrastructure
// how-to guide).
infrastructure: {
	state: turso: url: "http://127.0.0.1:8080"

	providers: random: {
		source:  "hashicorp/random"
		version: "3.9.1"
	}

	resources: {
		pet: {
			type: "random_pet"
			configuration: {
				length:    2
				separator: "-"
			}
			// Declared outputs can be read by other resources. The type
			// tells cuenv what the provider will return.
			outputs: id: type: "string"
		}
		database_password: {
			type: "random_password"
			configuration: {
				length:  24
				special: false
			}
		}
		port: {
			type: "random_integer"
			configuration: {
				min: 8000
				max: 8999
				// Rotates whenever the pet's name changes. The reference is
				// resolved at plan time; plain `cue export` sees a descriptor.
				keepers: pet: schema.#Format & {
					parts: ["rotate-with-", infrastructure.resources.pet.outputs.id]
				}
			}
		}
	}
}
