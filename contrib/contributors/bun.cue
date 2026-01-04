package contributors

import "github.com/cuenv/cuenv/schema"

// #BunWorkspace installs Bun dependencies when a bun.lock file is detected.
//
// Active when:
// - Project is a member of a Bun workspace (detected via workspaceMember condition)
//
// Injects tasks:
// - cuenv:contributor:bun.workspace.install: Runs bun install --frozen-lockfile
// - cuenv:contributor:bun.workspace.setup: Depends on install, used as dependency anchor
//
// Auto-associates:
// - Tasks using "bun" or "bunx" commands automatically depend on bun.workspace.setup
//
// Usage:
//
//	import "github.com/cuenv/cuenv/contrib/contributors"
//
//	ci: contributors: [contributors.#BunWorkspace]
#BunWorkspace: schema.#Contributor & {
	id: "bun.workspace"
	when: workspaceMember: ["bun"]
	tasks: [
		{
			id:          "bun.workspace.install"
			label:       "Install Bun dependencies"
			command:     "bun"
			args:        ["install", "--frozen-lockfile"]
			inputs:      ["package.json", "bun.lock"]
			outputs:     ["node_modules"]
			hermetic:    false
			description: "Install Bun dependencies from bun.lock"
		},
		{
			id:          "bun.workspace.setup"
			script:      "true"
			hermetic:    false
			dependsOn:   ["bun.workspace.install"]
			description: "Bun workspace setup complete"
		},
	]
	autoAssociate: {
		command:          ["bun", "bunx"]
		injectDependency: "cuenv:contributor:bun.workspace.setup"
	}
}

