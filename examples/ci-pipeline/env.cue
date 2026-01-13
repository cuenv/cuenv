package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

let _t = tasks

name: "ci-pipeline"

ci: pipelines: {
	default: {
		tasks: [_t.test]
	}
}

tasks: {
	test: schema.#Task & {
		command: "echo"
		args: ["Running test task"]
		inputs: ["env.cue"]
	}
}
