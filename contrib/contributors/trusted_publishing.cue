package contributors

import "github.com/cuenv/cuenv/schema"

// #TrustedPublishing enables OIDC-based trusted publishing for crates.io.
//
// Active when:
// - ci.provider.github.trustedPublishing.cratesIo is true
//
// Injects tasks:
// - cuenv:contributor:trusted-publishing.auth: Authenticates with crates.io
//
// Uses the rust-lang/crates-io-auth-action to obtain a short-lived token.
// This is a GitHub-specific contributor.
//
// Usage:
//
//	import "github.com/cuenv/cuenv/contrib/contributors"
//
//	ci: contributors: [contributors.#TrustedPublishing]
//	ci: provider: github: trustedPublishing: cratesIo: true
#TrustedPublishing: schema.#Contributor & {
	id: "trusted-publishing"
	when: providerConfig: ["github.trustedPublishing.cratesIo"]
	tasks: [{
		id:       "trusted-publishing.auth"
		label:    "Authenticate with crates.io"
		priority: 50
		command:  "sh"
		args:     ["-c", "echo 'Trusted publishing is only available on GitHub Actions'; exit 1"]
		provider: github: uses: "rust-lang/crates-io-auth-action@v1"
	}]
}

// #CueRegistryTrustedPublishing enables OIDC-based trusted publishing for the
// CUE Registry in GitHub release workflows.
//
// Active when:
// - ci.provider.github.trustedPublishing.cueRegistry is true
// - the pipeline environment is "production"
//
// The action is scoped to the conventional `publish.cue` job so the login is
// available before `cue mod publish` without minting a token in build jobs.
#CueRegistryTrustedPublishing: schema.#Contributor & {
	id: "trusted-publishing.cue-registry"
	when: {
		providerConfig: ["github.trustedPublishing.cueRegistry"]
		environment:    ["production"]
	}
	tasks: [{
		id:       "trusted-publishing.cue-registry.auth"
		label:    "Authenticate with the CUE Registry"
		priority: 25
		command:  "sh"
		args:     ["-c", "echo 'Trusted publishing is only available on GitHub Actions'; exit 1"]
		provider: github: {
			uses: "cue-labs/registry-login-action@v1"
			if:   "github.job == 'publish-cue'"
		}
	}]
}
