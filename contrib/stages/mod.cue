// Package stages provides built-in contributors for CI pipelines.
//
// Contributors inject tasks into build phases (bootstrap, setup, success, failure)
// based on activation conditions. This replaces hardcoded Rust Contributor
// implementations with declarative CUE definitions.
//
// Core Contributors (provider-agnostic):
//   - #Nix: Installs Nix via Determinate Systems installer
//   - #Cuenv: Installs or builds cuenv (multiple modes: release, git, nix, homebrew)
//   - #OnePassword: Sets up 1Password WASM SDK for secret resolution
//
// GitHub-Specific Contributors:
//   - #Cachix: Configures Cachix for Nix binary caching
//   - #GhModels: Installs GitHub Models CLI extension
//   - #TrustedPublishing: Enables OIDC-based crates.io authentication
//
// Usage:
//
//	import stages "github.com/cuenv/cuenv/contrib/stages"
//
//	ci: contributors: [
//	    stages.#Nix,
//	    stages.#Cuenv,
//	    stages.#OnePassword,
//	    stages.#Cachix,
//	    stages.#GhModels,
//	    stages.#TrustedPublishing,
//	]
//
// Or use the default set:
//
//	ci: contributors: stages.#DefaultContributors
package stages

import (
	"list"

	"github.com/cuenv/cuenv/schema"
)

// #CoreContributors contains the core (provider-agnostic) contributors.
// These are always evaluated regardless of the CI provider.
#CoreContributors: [...schema.#Contributor] & [
	#Nix,
	#Cuenv,
	#OnePassword,
]

// #GitHubContributors contains GitHub-specific contributors.
// These are only evaluated when using GitHub Actions as the CI provider.
#GitHubContributors: [...schema.#Contributor] & [
	#Cachix,
	#GhModels,
	#TrustedPublishing,
]

// #DefaultContributors contains all default contributors.
// Combines core contributors with GitHub-specific contributors.
#DefaultContributors: [...schema.#Contributor] & list.Concat([#CoreContributors, #GitHubContributors])
