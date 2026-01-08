package examples

import "github.com/cuenv/cuenv/schema"
import xCodecov "github.com/cuenv/cuenv/contrib/codecov"

schema.#Project

name: "ci-codecov"

ci: {
	contributors: [xCodecov.#Codecov]
	pipelines: {
		test: {
			tasks: ["test"]
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
