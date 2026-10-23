// want: comand: field not allowed
// min-language: v0.18.0
package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "reject"

tasks: build: {command: "cargo", comand: "build"}
