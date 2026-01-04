package contributors

import "github.com/cuenv/cuenv/schema"

// #Cachix sets up Cachix for Nix binary caching.
//
// Active when:
// - ci.provider.github.cachix is configured
//
// Injects tasks:
// - cuenv:contributor:cachix.setup: Sets up Cachix for binary caching
//
// This is a GitHub-specific contributor.
//
// Usage:
//
//	import "github.com/cuenv/cuenv/contrib/contributors"
//
//	ci: contributors: [contributors.#Cachix]
//	ci: provider: github: cachix: name: "my-cache"
#Cachix: schema.#Contributor & {
	id: "cachix"
	when: providerConfig: ["github.cachix"]
	tasks: [{
		id:        "cachix.setup"
		label:     "Setup Cachix"
		priority:  15
		dependsOn: ["nix.install"]
		env: {
			CACHIX_CACHE_NAME: "${CACHIX_CACHE_NAME}"
			CACHIX_AUTH_TOKEN: "${CACHIX_AUTH_TOKEN}"
		}
		secrets: CACHIX_AUTH_TOKEN: "CACHIX_AUTH_TOKEN"
		command: "sh"
		args: ["-c", ". /nix/var/nix/profiles/default/etc/profile.d/nix-daemon.sh && nix-env -iA cachix -f https://cachix.org/api/v1/install && cachix use ${CACHIX_CACHE_NAME}"]
	}]
}
