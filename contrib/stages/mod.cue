// Package stages provides built-in stage contributors for CI pipelines.
//
// Stage contributors inject tasks into build stages (bootstrap, setup, success, failure)
// based on activation conditions. This replaces hardcoded Rust StageContributor
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
//	ci: stageContributors: [
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
//	ci: stageContributors: stages.#DefaultContributors
package stages

import "github.com/cuenv/cuenv/schema"

// #CoreContributors contains the core (provider-agnostic) stage contributors.
// These are always evaluated regardless of the CI provider.
#CoreContributors: [...schema.#StageContributor] & [
	#Nix,
	#Cuenv,
	#OnePassword,
]

// #GitHubContributors contains GitHub-specific stage contributors.
// These are only evaluated when using GitHub Actions as the CI provider.
#GitHubContributors: [...schema.#StageContributor] & [
	#Cachix,
	#GhModels,
	#TrustedPublishing,
]

// #DefaultContributors contains all default stage contributors.
// Combines core contributors with GitHub-specific contributors.
#DefaultContributors: [...schema.#StageContributor] & (
	#CoreContributors + #GitHubContributors
)
