package schema

// Ignore patterns for tool-specific ignore files.
// Keys are tool names (e.g., "git", "docker", "prettier").
// Values can be either:
//   - A list of patterns: ["node_modules/", ".env"]
//   - An object with patterns and optional filename override
//
// Examples:
//   ignore: {
//       git: ["node_modules/", ".env"]  // generates .gitignore
//       docker: ["node_modules/", ".git/"]  // generates .dockerignore
//       custom: {
//           patterns: ["*.tmp", "cache/"]
//           filename: ".myignore"  // override default .<tool>ignore
//       }
//   }
#IgnoreEntry: {
	patterns!: [...string]
	filename?: string
}

#Ignore: {
	[string]: [...string] | #IgnoreEntry
}
