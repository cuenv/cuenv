package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "ci-gh-models"

// Pipeline that uses GitHub Models CLI for LLM evaluation
// This triggers the GhModelsContributor to inject extension setup
ci: pipelines: {
	eval: {
		tasks: ["eval.prompts"]
		when: branch: "main"
	}
}

tasks: {
	"eval.prompts": {
		command: "gh"
		args: ["models", "eval", "prompts/test.yml"]
		inputs: ["prompts/**/*.yml"]
	}
}
