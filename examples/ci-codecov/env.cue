package examples

import "github.com/cuenv/cuenv/schema"
import xCodecov "github.com/cuenv/cuenv/contrib/codecov"

schema.#Project

name: "ci-codecov"

ci: {
	contributors: [xCodecov.#Codecov]
	pipelines: [{
		name: "test"
		tasks: ["test"]
		when: pullRequest: true
	}]
}

tasks: {
	test: {
		command: "echo"
		args: ["Running tests with coverage"]
		labels: ["test"]
	}
}
