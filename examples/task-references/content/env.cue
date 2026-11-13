package examples

import "github.com/cuenv/cuenv/schema"

// A project whose task builds a file another project reads. It declares the
// file in `outputs`; it says nothing about who reads it.
schema.#Project

name: "task-references-content"

tasks: {
	build: schema.#Task & {
		command: "sh"
		args: ["-c", "mkdir -p dist && cp articles/hello.md dist/hello.txt"]
		inputs: ["articles/**"]
		outputs: ["dist/hello.txt"]
	}
}
