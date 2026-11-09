package examples

import (
	"github.com/cuenv/cuenv/schema"
	c "github.com/cuenv/cuenv/contrib/contributors"
)

schema.#Project

let _t = tasks

name: "ci-gh-models"

// Pipeline that uses GitHub Models CLI for LLM evaluation
// This triggers the GhModelsContributor to inject extension setup
ci: {
	providers: ["github"]
	contributors: [c.#Cuenv, c.#GhModels]
	pipelines: {
		eval: {
			tasks: [_t.evalPrompts]
			when: branch: "main"
		}
	}
}

tasks: {
	evalPrompts: schema.#Task & {
		command: "gh"
		args: ["models", "eval", "prompts/test.yml"]
		inputs: ["prompts/**/*.yml"]
	}
}
