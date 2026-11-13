package cuenv

import (
	"github.com/cuenv/cuenv/schema"
	sources "example.com/consumer/sources:cuenv"
)

schema.#Project

name: "content"

tasks: {
	// Reads the files `sources.tasks.pull` declares, and so depends on it.
	build: {
		command: "true"
		inputs: ["articles/**", sources.tasks.pull.outputFiles]
		outputs: ["dist/site.json"]
	}
	checks: {
		type: "group"
		lint: {command: "true", inputs: ["articles/**"]}
	}
}
