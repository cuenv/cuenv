// A task env written as a struct literal must stay #EnvPassthrough.
// #Secret forbids cuenvPassthrough, so this is one concrete value. If the
// forbid is dropped, the disjunction with #Secret is incomplete and export fails.
package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "accept"

tasks: show: {
	command: "env"
	env: CI_TOKEN: {cuenvPassthrough: true, name: "CI_TOKEN"}
}
