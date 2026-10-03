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
// This example keeps `configuration` untyped so it evaluates without network
// access. In a real project, import the provider's schema module from the CUE
// registry and unify it, for example:
//
//   import randomProvider "github.com/cuenv/terraform/terraform/hashicorp/random@v3"
//   configuration: randomProvider.#Resource_random_pet & {length: 2}
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
		}
		database_password: {
			type: "random_password"
			configuration: {
				length:  24
				special: false
			}
		}
		port: {
			type:      "random_integer"
			dependsOn: ["pet"]
			configuration: {
				min: 8000
				max: 8999
				keepers: pet: "rotate-with-pet"
			}
		}
	}
}
