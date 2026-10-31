// want: typo: field not allowed
package compat

import "github.com/cuenv/cuenv/schema"

locator: schema.#InfrastructureLocator & {
	directory: "infra"
	"package": "infrastructure"
	typo: true
}
