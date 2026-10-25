// Fields that take free-form values must keep accepting them when the
// project embeds schema.#Project at file level (CUE v0.17 closes `_` there).
package compat

import (
	"github.com/cuenv/cuenv/schema"
	gen "github.com/cuenv/cuenv/schema/codegen"
)

schema.#Project

name: "accept"

codegen: {
	context: {
		service: {name: "users", ports: [8080, 9090]}
		debug: true
	}
	files: "config.json": gen.#JSONFile & {
		mode:    "managed"
		content: "{\"name\": \"\(context.service.name)\"}"
	}
}

ci: {
	provider: buildkite: plugins: [{
		name: "docker-compose#v5.0.0"
		config: {run: "app", config: ["docker-compose.yml"]}
	}]
	contributors: [{
		id: "setup"
		tasks: [{
			id:      "toolchain"
			command: "true"
			provider: github: {
				uses: "actions/setup-go@v5"
				with: {"go-version": "1.24", "check-latest": true, "retries": 3}
			}
		}]
	}]
}

config: backend: {type: "dagger", options: image: "alpine:3"}
