package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "bun-workspace-test"

tasks: {
	build: {
		command: "echo"
		args: ["build"]
	}
}
