package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

let _t = tasks

name: "ci-aws-secrets"

// Environment with AWS Secrets Manager references.
// The runtime resolver uses the AWS CLI's standard credential chain.
env: {
	environment: production: {
		API_TOKEN: schema.#AwsSecret & {
			secretId: "prod/api-token"
		}
		DATABASE_PASSWORD: schema.#AwsSecret & {
			secretId: "prod/database"
			jsonKey:  "password"
		}
	}
}

tasks: deploy: schema.#Task & {
	command: "echo"
	args:    ["Deploying with secrets from AWS Secrets Manager"]
	inputs:  ["env.cue"]
}

ci: pipelines: deploy: {
	environment: "production"
	tasks:       [_t.deploy]
	when: branch: "main"
}
