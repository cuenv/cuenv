package _examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "ci-onepassword"

// Environment with 1Password secret references
// This triggers the OnePasswordContributor to inject setup steps
env: {
	production: {
		API_TOKEN:  schema.#OnePasswordRef & {ref: "op://vault/api/token"}
		DEPLOY_KEY: schema.#OnePasswordRef & {ref: "op://vault/deploy/key"}
	}
}

tasks: {
	deploy: {
		command: "echo"
		args: ["Deploying with secrets from 1Password"]
		inputs: ["env.cue"]
	}
}

ci: pipelines: [
	{
		name:        "deploy"
		environment: "production"
		tasks: ["deploy"]
		when: branch: "main"
	},
]
