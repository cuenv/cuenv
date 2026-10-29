package examples

import (
	"github.com/cuenv/cuenv/schema"
	xContributors "github.com/cuenv/cuenv/contrib/contributors"
)

schema.#Project

let _t = tasks

name: "ci-infisical-action"

// Pattern B: Infisical's official GitHub Action logs in with OIDC and exports
// every secret under /deploy as a job environment variable. The task reads
// the one it needs with #EnvPassthrough. cuenv does not resolve, scope or
// redact these values itself; the action masks them in GitHub's logs.
tasks: deploy: schema.#Task & {
	command: "echo"
	args: ["Deploying with secrets from Infisical/secrets-action"]
	inputs: ["env.cue"]
	env: API_TOKEN: schema.#EnvPassthrough
}

ci: {
	providers: ["github"]
	contributors: [
		xContributors.#Cuenv,
		xContributors.#InfisicalSecretsAction & {
			#project: slug: "my-project"
			#envSlug:    "prod"
			#secretPath: "/deploy"
		},
	]
	pipelines: deploy: {
		tasks: [_t.deploy]
		when: branch: "main"
	}
}
