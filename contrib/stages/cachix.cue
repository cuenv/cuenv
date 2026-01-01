package stages

import "github.com/cuenv/cuenv/schema"

// #Cachix sets up Cachix for Nix binary caching.
//
// Active when:
// - ci.provider.github.cachix is configured
//
// Contributes to Setup stage with priority 15 (after Nix install, before cuenv).
// Depends on install-nix to have run first.
//
// This is a GitHub-specific contributor.
//
// Usage:
//
//	import stages "github.com/cuenv/cuenv/contrib/stages"
//
//	ci: stageContributors: [stages.#Cachix]
//	ci: provider: github: cachix: name: "my-cache"
#Cachix: schema.#StageContributor & {
	id: "cachix"
	when: {
		// Active if Cachix is configured in GitHub provider
		providerConfig: ["github.cachix"]
	}
	tasks: [{
		id:        "setup-cachix"
		stage:     "setup"
		label:     "Setup Cachix"
		priority:  15
		shell:     true
		dependsOn: ["install-nix"]
		env: {
			CACHIX_CACHE_NAME:  "${CACHIX_CACHE_NAME}"
			CACHIX_AUTH_TOKEN:  "${CACHIX_AUTH_TOKEN}"
		}
		secrets: CACHIX_AUTH_TOKEN: "CACHIX_AUTH_TOKEN"
		// Command template - actual cache name is substituted at runtime
		command: """
			. /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh && \\
			nix-env -iA cachix -f https://cachix.org/api/v1/install && \\
			cachix use ${CACHIX_CACHE_NAME}
			"""
	}]
}
