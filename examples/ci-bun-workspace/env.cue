package examples

import (
	"github.com/cuenv/cuenv/schema"
	xBun "github.com/cuenv/cuenv/contrib/bun"
	c "github.com/cuenv/cuenv/contrib/contributors"
)

schema.#Project

name: "ci-bun-workspace"

runtime: schema.#ToolsRuntime & {
	platforms: ["linux-x86_64", "darwin-arm64"]
	tools: {
		bun: xBun.#Bun & {version: "1.1.0"}
	}
}

ci: {
	contributors: [c.#Cuenv, c.#Bun]
	pipelines: [{
		name:  "default"
		tasks: ["version"]
		when: branch: "main"
	}]
}

env: {}

tasks: {
	version: {
		command: "bun"
		args: ["--version"]
	}
}
