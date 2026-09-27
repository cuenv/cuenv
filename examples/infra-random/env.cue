package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "infra-random"

// Managed resources driven through the hashicorp/random provider plugin.
// State lives in Turso, keyed by this CUE module path and project name.
//
//   sqld --http-listen-addr 127.0.0.1:8080   # or point url at libsql://...
//   cuenv infra plan  -p examples/infra-random --package examples
//   cuenv infra apply -p examples/infra-random --package examples
infra: {
	state: turso: url: "http://127.0.0.1:8080"

	providers: random: {
		source:  "hashicorp/random"
		version: "3.7.2"
	}

	resources: {
		pet: {
			type: "random_pet"
			config: {
				length:    2
				separator: "-"
			}
		}
		db_password: {
			type: "random_password"
			config: {
				length:  24
				special: false
			}
		}
		port: {
			type:      "random_integer"
			dependsOn: ["pet"]
			config: {
				min: 8000
				max: 8999
				keepers: pet: "rotate-with-pet"
			}
		}
	}
}
