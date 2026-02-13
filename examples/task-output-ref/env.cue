package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "task-output-ref"

tasks: {
	// Create a temporary directory — its stdout is the path
	tmpdir: schema.#Task & {
		command: "mktemp"
		args: ["-d"]
	}

	// Use stdout of tmpdir as an environment variable
	work: schema.#Task & {
		command: "echo"
		args: ["working in", tasks.tmpdir.stdout]
		// No explicit dependsOn needed — auto-inferred from the output reference
	}

	// Use stdout of tmpdir directly in args
	cleanup: schema.#Task & {
		command: "rm"
		args: ["-rf", tasks.tmpdir.stdout]
		dependsOn: [work]
	}

	// Sequence with output references between steps
	pipeline: schema.#TaskSequence & [
		schema.#Task & {command: "echo", args: ["-n", "hello-from-pipeline"]},
		schema.#Task & {command: "echo", args: ["received:", pipeline[0].stdout]},
	]
}
