package examples

import "github.com/cuenv/cuenv/schema"

schema.#Cuenv

env: {
	NAME: "Jack O'Neill"
}

tasks: {
	// Simple Task
	interpolate: {
		shell:   "bash"
		command: "echo"
		args: ["Hello ", env.NAME, "!"]
	}

	propagate: {
		shell:   "bash"
		command: "printenv"
		// Good test-case to ensure env above is available at execution.
		args: ["NAME"]
	}

	// Task List
	greetAll: [
		{
			shell:   "bash"
			command: "echo"
			args: ["Hello 1 ", env.NAME, "!"]
		},
		{
			shell:   "bash"
			command: "echo"
			args: ["Hello 2 ", env.NAME, "!"]
		},
	]

	// Nested Tasks
	greetIndividual: {jack: {
		shell:   "bash"
		command: "echo"
		args: ["Hello Jack"]
	}
		tealc: {
			shell:   "bash"
			command: "echo"
			args: ["Hello Teal'c"]
		}
	}
}
