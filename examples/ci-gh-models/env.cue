package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

let _t = tasks

name: "ci-gh-models"

// Pipeline that uses GitHub Models CLI for LLM evaluation
// This triggers the GhModelsContributor to inject extension setup
ci: pipelines: {
	eval: {
		tasks: [_t.evalPrompts]
		when: branch: "main"
	}
}

tasks: {
	evalPrompts: schema.#Task & {
		command: "gh"
		args: ["models", "eval", "prompts/test.yml"]
		inputs: ["prompts/**/*.yml"]
	}
}
