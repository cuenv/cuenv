package schema

// Predefined workspace types for common package managers.
// Use these with: workspaces: bun: #BunWorkspace

#BunWorkspace: #WorkspaceConfig & {
	commands: ["bun", "bunx"]
	inject: {
		install: {
			command:  "bun"
			args:     ["install"]
			hermetic: false
			inputs:   ["package.json", "bun.lock"]
			outputs:  ["node_modules"]
		}
	}
}

#NpmWorkspace: #WorkspaceConfig & {
	commands: ["npm", "npx"]
	inject: {
		install: {
			command:  "npm"
			args:     ["install"]
			hermetic: false
			inputs:   ["package.json", "package-lock.json"]
			outputs:  ["node_modules"]
		}
	}
}

#PnpmWorkspace: #WorkspaceConfig & {
	commands: ["pnpm", "pnpx"]
	inject: {
		install: {
			command:  "pnpm"
			args:     ["install"]
			hermetic: false
			inputs:   ["package.json", "pnpm-lock.yaml"]
			outputs:  ["node_modules"]
		}
	}
}

#YarnWorkspace: #WorkspaceConfig & {
	commands: ["yarn"]
	inject: {
		install: {
			command:  "yarn"
			args:     ["install"]
			hermetic: false
			inputs:   ["package.json", "yarn.lock"]
			outputs:  ["node_modules"]
		}
	}
}

#CargoWorkspace: #WorkspaceConfig & {
	commands: ["cargo"]
	// No install task for Cargo - dependencies are resolved during build
	inject: {}
}

#GoWorkspace: #WorkspaceConfig & {
	commands: ["go"]
	inject: {
		download: {
			command:  "go"
			args:     ["mod", "download"]
			hermetic: false
			inputs:   ["go.mod", "go.sum"]
			outputs:  []
		}
	}
}

#DenoWorkspace: #WorkspaceConfig & {
	commands: ["deno"]
	inject: {
		cache: {
			command:  "deno"
			args:     ["cache", "**/*.ts"]
			hermetic: false
			inputs:   ["deno.json", "deno.lock"]
			outputs:  []
		}
	}
}
