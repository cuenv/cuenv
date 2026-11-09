package examples

import (
	"github.com/cuenv/cuenv/schema"
	xCodecov "github.com/cuenv/cuenv/contrib/codecov"
	c "github.com/cuenv/cuenv/contrib/contributors"
)

schema.#Project

let _t = tasks

name: "ci-codecov"

ci: {
	providers: ["github"]
	contributors: [c.#Cuenv, xCodecov.#Codecov]
	pipelines: {
		test: {
			tasks: [_t.test]
			when: pullRequest: true
		}
	}
}

tasks: {
	test: schema.#Task & {
		command: "echo"
		args: ["Running tests with coverage"]
		labels: ["test"]
	}
}
