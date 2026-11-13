package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "sources"

// The first link of a chain of three projects, each depending on a task of
// the one before through its import.
tasks: pull: {
	command: "true"
	inputs: ["feeds/**"]
	outputs: ["raw.json"]
}
