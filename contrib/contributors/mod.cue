// Package contributors provides built-in contributors for CI pipelines.
//
// Contributors inject tasks into the DAG based on activation conditions.
// The ContributorEngine applies these contributors before task execution,
// both for CLI (`cuenv task`) and CI (`cuenv ci`) paths.
//
// Workspace Contributors (auto-detect from lockfiles):
//   - #BunWorkspace: Installs Bun dependencies (auto-detects bun.lock)
//   - #NpmWorkspace: Installs npm dependencies (auto-detects package-lock.json)
//
// Runtime Contributors:
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
//	import "github.com/cuenv/cuenv/contrib/contributors"
//
//	ci: contributors: [
//	    contributors.#Nix,
//	    contributors.#Cuenv,
//	    contributors.#BunWorkspace,
//	    contributors.#NpmWorkspace,
//	    contributors.#OnePassword,
//	    contributors.#Cachix,
//	    contributors.#GhModels,
//	    contributors.#TrustedPublishing,
//	]
//
// Or use the default set:
//
//	ci: contributors: contributors.#DefaultContributors
package contributors

import (
	"list"

	"github.com/cuenv/cuenv/schema"
)

// #WorkspaceContributors contains workspace-related contributors.
// These detect package managers from lockfiles and inject install tasks.
#WorkspaceContributors: [...schema.#Contributor] & [
	#BunWorkspace,
	#NpmWorkspace,
]

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
// Combines workspace, core, and GitHub-specific contributors.
#DefaultContributors: [...schema.#Contributor] & list.Concat([#WorkspaceContributors, #CoreContributors, #GitHubContributors])
