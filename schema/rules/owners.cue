package rules

// #RulesOwners - simplified owners for directory rules (no output config)
// Rules are aggregated across all .rules.cue files to generate
// a single CODEOWNERS file at the repository root
#RulesOwners: {
	rules: [string]: #OwnerRule
}

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
