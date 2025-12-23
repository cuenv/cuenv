package schema

// #Target defines supported build targets for binary distribution
#Target: "linux-x64" | "linux-arm64" | "darwin-arm64"

// #GitHubBackend configures GitHub Releases as a distribution backend
#GitHubBackend: close({
	// Repository in "owner/repo" format (auto-detected from git remote if omitted)
	repo?: string
	// Upload binary tarballs as release assets (default: true)
	assets?: bool | *true
	// Create release as draft (default: false)
	draft?: bool | *false
})

// #HomebrewBackend configures Homebrew tap updates
#HomebrewBackend: close({
	// Tap repository in "owner/repo" format (required)
	tap!: string
	// Formula name (default: project name)
	formula?: string
	// Token env var name for pushing to tap (default: HOMEBREW_TAP_TOKEN)
	tokenEnv?: string | *"HOMEBREW_TAP_TOKEN"
})

// #CratesBackend configures crates.io publishing
#CratesBackend: close({
	// Token env var name (default: CARGO_REGISTRY_TOKEN)
	tokenEnv?: string | *"CARGO_REGISTRY_TOKEN"
	// Publish packages in dependency order (default: true)
	ordered?: bool | *true
})

// #CueBackend configures CUE registry publishing
#CueBackend: close({
	// Module path (auto-detected from cue.mod if omitted)
	module?: string
	// Token env var name (default: CUE_REGISTRY_TOKEN)
	tokenEnv?: string | *"CUE_REGISTRY_TOKEN"
})

// #ReleaseBackends defines which distribution backends to use
#ReleaseBackends: close({
	github?:   #GitHubBackend
	homebrew?: #HomebrewBackend
	crates?:   #CratesBackend
	cue?:      #CueBackend
})

// #Release defines the release management configuration for cuenv.
// This enables native release workflows including versioning, changelogs, and publishing.
#Release: close({
	// Binary name for distribution (default: project name)
	binary?: string

	// Build targets for binary distribution
	targets?: [...#Target]

	// Distribution backends configuration
	backends?: #ReleaseBackends

	// Git configuration for release management
	git?: #ReleaseGit

	// Package grouping configuration
	packages?: #ReleasePackages

	// CHANGELOG generation configuration
	changelog?: #ChangelogConfig
})

// #TagType defines the versioning scheme for release tags
#TagType: "semver" | "calver"

// #ReleaseGit defines git-related release settings
#ReleaseGit: close({
	// Default branch for releases (e.g., "main", "master")
	defaultBranch?: string | *"main"

	// Tag prefix for version tags (default: empty for bare versions)
	// Examples: "" → 0.19.1, "v" → v0.19.1, "vscode/v" → vscode/v0.1.1
	tagPrefix?: string | *""

	// Version tag type (default: semver)
	// - "semver": Semantic versioning (e.g., 0.19.1, 1.0.0-alpha.1)
	// - "calver": Calendar versioning (e.g., 2024.12.23, 24.04)
	tagType?: #TagType | *"semver"

	// Whether to create tags during release
	createTags?: bool | *true

	// Whether to push tags to remote
	pushTags?: bool | *true
})

// #VersioningStrategy defines how packages are versioned in a monorepo
#VersioningStrategy: "fixed" | "linked" | "independent"

// #ReleasePackages defines package grouping for version management
#ReleasePackages: close({
	// Default versioning strategy for packages not in explicit groups.
	// - "fixed": All packages share the same version (lockstep versioning)
	// - "linked": Packages are bumped together but can have different versions
	// - "independent": Each package is versioned independently (default)
	strategy?: #VersioningStrategy | *"independent"

	// Fixed groups: packages that share the same version (lockstep versioning)
	// All packages in a group are bumped together with the highest bump level.
	// Example: [["crates/cuenv-core", "crates/cuenv-cli"]]
	fixed?: [...[...string]]

	// Linked groups: packages that have their versions updated together when
	// any one of them has a change. Unlike fixed, versions can differ.
	// Example: [["crates/*"]]
	linked?: [...[...string]]

	// Independent packages: not part of any group, versioned independently
	// This is implicit - packages not in fixed or linked are independent
})

// #ChangelogConfig defines CHANGELOG generation settings
#ChangelogConfig: close({
	// Path to the CHANGELOG file relative to project/package root
	path?: string | *"CHANGELOG.md"

	// Whether to generate changelogs for each package
	perPackage?: bool | *true

	// Whether to generate a root changelog for the entire workspace
	workspace?: bool | *true

	// Categories for organizing changelog entries
	categories?: [...#ChangelogCategory]
})

// #ChangelogCategory defines a category for changelog entries
#ChangelogCategory: close({
	// Title for the category in the changelog
	title: string

	// Changeset types that belong to this category
	types: [...#ChangesetType]
})

// #ChangesetType defines the type of change in a changeset
#ChangesetType: "major" | "minor" | "patch" | "none"

// #Changeset represents a single changeset entry
#Changeset: close({
	// Unique identifier for the changeset
	id: string

	// Summary of the change
	summary: string

	// Packages affected by this change
	packages: [...#PackageChange]

	// Optional longer description
	description?: string
})

// #PackageChange represents a version bump for a specific package
#PackageChange: close({
	// Package name or path
	name: string

	// Type of version bump
	bump: #ChangesetType
})
