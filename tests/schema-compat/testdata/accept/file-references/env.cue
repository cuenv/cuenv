package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "accept"

// A module reference exports as plain data at every language version. Paths
// may start with a dot, hold `..` inside a segment, name a file whose name
// only starts or ends like `.git`, and use globs.
let _lock = schema.#ModuleFiles & {paths: ["bun.lock"]}

tasks: build: schema.#Task & {
	command: "cargo"
	inputs: [
		"src/**",
		_lock,
		schema.#ModuleFiles & {paths: [".github/**", "a..b/x", "src/**/*.{ts,tsx}", ".gitignore", "drafts.git"]},
	]
}
