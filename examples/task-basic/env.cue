package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "task-basic"

env: {
	NAME: "Jack O'Neill"
}

tasks: {
	// Simple Task
	interpolate: {
		command: "echo"
		args: ["Hello ", env.NAME, "!"]
	}

	propagate: {
		command: "printenv"
		// Good test-case to ensure env above is available at execution.
		args: ["NAME"]
	}

	// Task List
	greetAll: [
		{
			command: "echo"
			args: ["Hello 1 ", env.NAME, "!"]
		},
		{
			command: "echo"
			args: ["Hello 2 ", env.NAME, "!"]
		},
	]

	// Nested Tasks (Task Group)
	greetIndividual: {
		type: "group"
		jack: {
			command: "echo"
			args: ["Hello Jack"]
		}
		tealc: {
			command: "echo"
			args: ["Hello Teal'c"]
		}
	}

	// Shell Task
	shellExample: {
		shell: schema.#Bash
		command: "echo"
		args: ["Hello from Bash"]
	}
}
