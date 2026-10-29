package examples

import (
	"github.com/cuenv/cuenv/schema"
	xContributors "github.com/cuenv/cuenv/contrib/contributors"
)

schema.#Project

let _t = tasks

name: "ci-infisical-oidc"

// Pattern A: cuenv resolves the Infisical references itself and logs in with
// GitHub Actions OIDC, so no Infisical credential is stored in GitHub. The
// machine identity ID comes from the INFISICAL_MACHINE_IDENTITY_ID repository
// secret by default.
env: {
	environment: production: {
		API_TOKEN: schema.#InfisicalSecret & {
			projectId:   "00000000-0000-0000-0000-000000000000"
			environment: "prod"
			secretName:  "API_TOKEN"
		}
	}
}

tasks: deploy: schema.#Task & {
	command: "echo"
	args: ["Deploying with secrets from Infisical (OIDC)"]
	inputs: ["env.cue"]
}

ci: {
	providers: ["github"]
	contributors: [
		xContributors.#Cuenv,
		xContributors.#Infisical,
		xContributors.#InfisicalOidc & {#audience: "https://github.com/cuenv"},
	]
	pipelines: deploy: {
		environment: "production"
		tasks: [_t.deploy]
		when: branch: "main"
	}
}
