package contributors

import "github.com/cuenv/cuenv/schema"

// #Bun installs Bun dependencies when a bun.lock file is detected.
//
// Active when:
// - Project has a bun.lock file (detected via workspaceType condition)
//
// Contributes to Setup phase:
// - bun-install (priority 50): Runs bun install --frozen-lockfile
//
// Note: Bun itself is installed via cuenv's tools runtime (cuenv sync -A),
// so this contributor only handles dependency installation.
//
// Usage:
//
//	import "github.com/cuenv/cuenv/contrib/contributors"
//
//	ci: contributors: [contributors.#Bun]
#Bun: schema.#Contributor & {
	id: "bun"
	when: {
		workspaceType: ["bun"]
	}
	tasks: [{
		id:        "bun-install"
		phase:     "setup"
		label:     "Install Bun dependencies"
		priority:  50
		shell:     false
		dependsOn: ["setup-cuenv"]
		command:   "cuenv"
		args: ["exec", "--", "bun", "install", "--frozen-lockfile"]
	}]
}
