package schema

#Cuenv: {
	config?: #Config
	env?: #Env
	hooks?: #Hooks
	workspaces?: #Workspaces
	ci?: #CI
	release?: #Release
	tasks: [string]: #Tasks | *{}
}

#Workspaces: [string]: #WorkspaceConfig

#WorkspaceConfig: {
	enabled: bool | *true
	package_manager?: "npm" | "pnpm" | "yarn" | "yarn-classic" | "bun" | "cargo"
	root?: string
}
