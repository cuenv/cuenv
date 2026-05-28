package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

let _t = tasks

name: "ci-gcp-secret"

// Environment with Google Cloud Secret Manager references.
env: {
	environment: production: {
		API_TOKEN: schema.#GcpSecret & {
			project: "my-gcp-project"
			secret:  "api-token"
		}
		DEPLOY_KEY: schema.#GcpSecret & {
			project: "my-gcp-project"
			secret:  "deploy-key"
			version: "5"
		}
	}
}

tasks: deploy: schema.#Task & {
	command: "echo"
	args:    ["Deploying with secrets from Google Cloud Secret Manager"]
	inputs:  ["env.cue"]
}

ci: pipelines: deploy: {
	environment: "production"
	tasks:       [_t.deploy]
	when: branch: "main"
}
