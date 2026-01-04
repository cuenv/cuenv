package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "npm-workspace-test"

tasks: {
	build: {
		command: "echo"
		args: ["build"]
	}
}
