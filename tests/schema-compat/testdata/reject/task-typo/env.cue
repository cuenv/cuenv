// want: comand: field not allowed
// Before min-language a top-level typo in a tasks entry written without
// schema.#Task & is accepted. The compat check requires that.
package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "reject"

tasks: build: {command: "cargo", comand: "build"}
