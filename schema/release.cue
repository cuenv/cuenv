package schema

// #Release defines the release management configuration for cuenv.
// This enables native release workflows including versioning, changelogs, and publishing.
#Release: {
	// Git configuration for release management
	git?: #ReleaseGit

	// Package grouping configuration
	packages?: #ReleasePackages

	// CHANGELOG generation configuration
	changelog?: #ChangelogConfig
}

// #ReleaseGit defines git-related release settings
#ReleaseGit: {
	// Default branch for releases (e.g., "main", "master")
	defaultBranch?: string | *"main"

	// Tag format template. Supports ${package} and ${version} placeholders.
	// Examples:
	//   - "v${version}" for single-package repos
	//   - "${package}-v${version}" for monorepos
	tagFormat?: string | *"v${version}"

	// Whether to create tags during release
	createTags?: bool | *true

	// Whether to push tags to remote
	pushTags?: bool | *true
}

// #ReleasePackages defines package grouping for version management
#ReleasePackages: {
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
}

// #ChangelogConfig defines CHANGELOG generation settings
#ChangelogConfig: {
	// Path to the CHANGELOG file relative to project/package root
	path?: string | *"CHANGELOG.md"

	// Whether to generate changelogs for each package
	perPackage?: bool | *true

	// Whether to generate a root changelog for the entire workspace
	workspace?: bool | *true

	// Categories for organizing changelog entries
	categories?: [...#ChangelogCategory]
}

// #ChangelogCategory defines a category for changelog entries
#ChangelogCategory: {
	// Title for the category in the changelog
	title: string

	// Changeset types that belong to this category
	types: [...#ChangesetType]
}

// #ChangesetType defines the type of change in a changeset
#ChangesetType: "major" | "minor" | "patch" | "none"

// #Changeset represents a single changeset entry
#Changeset: {
	// Unique identifier for the changeset
	id: string

	// Summary of the change
	summary: string

	// Packages affected by this change
	packages: [...#PackageChange]

	// Optional longer description
	description?: string
}

// #PackageChange represents a version bump for a specific package
#PackageChange: {
	// Package name or path
	name: string

	// Type of version bump
	bump: #ChangesetType
}
