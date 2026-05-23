package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

let _t = tasks

name: "ci-infisical"

// Environment with Infisical REST API secret references.
// This triggers the Infisical contributor to inject setup/preflight steps.
env: {
	environment: production: {
		API_TOKEN: schema.#InfisicalSecret & {
			projectId:   "00000000-0000-0000-0000-000000000000"
			environment: "prod"
			secretName:  "API_TOKEN"
		}
		DEPLOY_KEY: schema.#InfisicalSecret & {
			projectId:   "00000000-0000-0000-0000-000000000000"
			environment: "prod"
			secretName:  "DEPLOY_KEY"
			secretPath:  "/deploy"
		}
	}
}

tasks: deploy: schema.#Task & {
	command: "echo"
	args:    ["Deploying with secrets from Infisical"]
	inputs:  ["env.cue"]
}

ci: pipelines: deploy: {
	environment: "production"
	tasks:       [_t.deploy]
	when: branch: "main"
}
