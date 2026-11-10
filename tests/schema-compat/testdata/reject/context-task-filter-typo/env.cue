// want: comand: field not allowed
package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "reject"

contexts: bun: schema.#TaskFilter & {comand: "bun"}
