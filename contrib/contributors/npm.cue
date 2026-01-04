package contributors

import "github.com/cuenv/cuenv/schema"

// #NpmWorkspace installs npm dependencies when a package-lock.json file is detected.
//
// Active when:
// - Project is a member of an npm workspace (detected via workspaceMember condition)
//
// Injects tasks:
// - cuenv:contributor:npm.workspace.install: Runs npm ci
// - cuenv:contributor:npm.workspace.setup: Depends on install, used as dependency anchor
//
// Auto-associates:
// - Tasks using "npm" or "npx" commands automatically depend on npm.workspace.setup
//
// Usage:
//
//	import "github.com/cuenv/cuenv/contrib/contributors"
//
//	ci: contributors: [contributors.#NpmWorkspace]
#NpmWorkspace: schema.#Contributor & {
	id: "npm.workspace"
	when: workspaceMember: ["npm"]
	tasks: [
		{
			id:          "npm.workspace.install"
			label:       "Install npm dependencies"
			command:     "npm"
			args:        ["ci"]
			inputs:      ["package.json", "package-lock.json"]
			outputs:     ["node_modules"]
			hermetic:    false
			description: "Install npm dependencies from package-lock.json"
		},
		{
			id:          "npm.workspace.setup"
			script:      "true"
			hermetic:    false
			dependsOn:   ["npm.workspace.install"]
			description: "npm workspace setup complete"
		},
	]
	autoAssociate: {
		command:          ["npm", "npx"]
		injectDependency: "cuenv:contributor:npm.workspace.setup"
	}
}
