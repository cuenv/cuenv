package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "task-basic"

env: {
	NAME: "Jack O'Neill"
}

tasks: {
	// Simple Task with explicit #Task type
	interpolate: schema.#Task & {
		command: "echo"
		args: ["Hello ", env.NAME, "!"]
	}

	propagate: schema.#Task & {
		command: "printenv"
		// Good test-case to ensure env above is available at execution.
		args: ["NAME"]
	}

	// Task Sequence - steps run in order
	greetAll: schema.#TaskSequence & [
		schema.#Task & {
			command: "echo"
			args: ["Hello 1 ", env.NAME, "!"]
		},
		schema.#Task & {
			command: "echo"
			args: ["Hello 2 ", env.NAME, "!"]
		},
	]

	// Task Group - children run in parallel
	greetIndividual: schema.#TaskGroup & {
		type: "group"
		jack: schema.#Task & {
			command: "echo"
			args: ["Hello Jack"]
		}
		tealc: schema.#Task & {
			command: "echo"
			args: ["Hello Teal'c"]
		}
	}

	// Shell Task with explicit scriptShell
	shellExample: schema.#Task & {
		scriptShell: "bash"
		command: "echo"
		args: ["Hello from Bash"]
	}
}
