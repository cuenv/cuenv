// want: infrastucture: field not allowed
package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "reject"

contexts: plan: schema.#When & {infrastucture: "plan"}
