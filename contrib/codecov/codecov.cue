package codecov

import "github.com/cuenv/cuenv/schema"

// #Codecov uploads coverage reports to Codecov after successful test runs.
//
// Active when:
// - A task with label "test" or "coverage" exists in the pipeline
//
// Contributes to Success phase:
// - codecov-upload (priority 50): Uploads coverage using Codecov CLI or GitHub Action
//
// Configuration:
// - Requires CODECOV_TOKEN secret
// - Automatically detects coverage files
//
// Usage:
//
//	import xCodecov "github.com/cuenv/cuenv/contrib/codecov"
//
//	ci: contributors: [xCodecov.#Codecov]
#Codecov: schema.#Contributor & {
	id: "codecov"
	when: {
		taskLabels: ["test", "coverage"]
	}
	tasks: [{
		id:        "codecov-upload"
		phase:     "success"
		label:     "Upload coverage to Codecov"
		priority:  50
		condition: "on_success"
		shell:     false
		command:   "codecov"
		args: ["upload-process", "--auto-detect"]
		env: {
			CODECOV_TOKEN: "${CODECOV_TOKEN}"
		}
		provider: github: {
			uses: "codecov/codecov-action@v5"
			with: {
				"token":            "${CODECOV_TOKEN}"
				"fail_ci_if_error": "false"
				"verbose":          "true"
			}
		}
	}]
}

// #CodecovOIDC uses OIDC authentication instead of a token.
//
// This variant uses OpenID Connect for authentication, eliminating the need
// for a CODECOV_TOKEN secret. Requires id-token: write permission in the workflow.
//
// Active when:
// - A task with label "test" or "coverage" exists in the pipeline
//
// Usage:
//
//	import xCodecov "github.com/cuenv/cuenv/contrib/codecov"
//
//	ci: contributors: [xCodecov.#CodecovOIDC]
#CodecovOIDC: schema.#Contributor & {
	id: "codecov-oidc"
	when: {
		taskLabels: ["test", "coverage"]
	}
	tasks: [{
		id:        "codecov-upload"
		phase:     "success"
		label:     "Upload coverage to Codecov (OIDC)"
		priority:  50
		condition: "on_success"
		shell:     false
		command:   "codecov"
		args: ["upload-process", "--auto-detect"]
		provider: github: {
			uses: "codecov/codecov-action@v5"
			with: {
				"use_oidc":         "true"
				"fail_ci_if_error": "false"
				"verbose":          "true"
			}
		}
	}]
}
