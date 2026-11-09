package examples

import (
	"github.com/cuenv/cuenv/schema"
	c "github.com/cuenv/cuenv/contrib/contributors"
	xCodecov "github.com/cuenv/cuenv/contrib/codecov"
)

schema.#Project

let _t = tasks

name: "ci-pipeline"

ci: {
	providers: ["github"]
	contributors: [
		c.#Cuenv,
		// These conditional contributors stay inactive without matching configuration.
		c.#Cachix, c.#OnePassword, c.#GhModels, c.#Hestia, xCodecov.#Codecov,
	]
	pipelines: {
		default: {
			tasks: [_t.test]
		}
	}
}

tasks: {
	test: schema.#Task & {
		command: "echo"
		args: ["Running test task"]
		inputs: ["env.cue"]
	}
}
