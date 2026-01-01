package stages

import "github.com/cuenv/cuenv/schema"

// #TrustedPublishing enables OIDC-based trusted publishing for crates.io.
//
// Active when:
// - ci.provider.github.trustedPublishing.cratesIo is true
//
// Contributes to Setup phase with priority 50 (runs late, before task execution).
// Uses the rust-lang/crates-io-auth-action to obtain a short-lived token.
//
// This is a GitHub-specific contributor.
//
// Usage:
//
//	import stages "github.com/cuenv/cuenv/contrib/stages"
//
//	ci: contributors: [stages.#TrustedPublishing]
//	ci: provider: github: trustedPublishing: cratesIo: true
#TrustedPublishing: schema.#Contributor & {
	id: "trusted-publishing"
	when: {
		// Active if trusted publishing for crates.io is enabled
		providerConfig: ["github.trustedPublishing.cratesIo"]
	}
	tasks: [{
		id:       "auth-crates-io"
		phase:    "setup"
		label:    "Authenticate with crates.io"
		priority: 50
		shell:    false
		// Empty command - uses GitHub Action instead
		provider: github: uses: "rust-lang/crates-io-auth-action@v1"
	}]
}
