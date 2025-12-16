package schema

// #Owners defines code ownership configuration for the project.
// This enables generating CODEOWNERS files for GitHub/GitLab.
#Owners: close({
	// Output configuration for CODEOWNERS file generation
	output?: #OwnersOutput

	// Global default owners applied to all patterns without explicit owners
	defaultOwners?: [...#Owner]

	// Code ownership rules - maps rule names to rule definitions
	// Using a map enables CUE unification/layering across configs
	rules: [string]: #OwnerRule
})

// #OwnersOutput configures where to write the CODEOWNERS file
#OwnersOutput: close({
	// Platform to generate CODEOWNERS for
	// - "github": writes to .github/CODEOWNERS
	// - "gitlab": writes to CODEOWNERS in root
	// - "bitbucket": writes to CODEOWNERS in root
	// Defaults to auto-detection based on repository structure
	platform?: "github" | "gitlab" | "bitbucket"

	// Custom path for CODEOWNERS file (overrides platform default)
	path?: string

	// Header comment to include at the top of the generated file
	header?: string
})

// #OwnerRule defines a single code ownership rule
#OwnerRule: close({
	// File pattern (glob syntax) - same as CODEOWNERS format
	// Examples: "*.js", "/docs/**", "src/lib/**/*.ts"
	pattern!: string

	// Owners for this pattern
	owners!: [...#Owner]

	// Optional description for this rule (added as comment above the rule)
	description?: string

	// Section name for grouping rules in the output file
	// Rules with the same section are grouped together with a header comment
	section?: string

	// Optional order for deterministic output (lower values appear first)
	// Rules without order are sorted alphabetically by key after ordered rules
	order?: int
})

// #Owner represents a code owner (user, team, or email)
#Owner: string & =~"^(@[a-zA-Z0-9_-]+(/[a-zA-Z0-9_-]+)?|[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\\.[a-zA-Z]{2,})$"
// Pattern breakdown:
// - @username: GitHub/GitLab user
// - @org/team-name: GitHub team or GitLab group
// - email@example.com: Email address
