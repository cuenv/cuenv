package schema

#Cuenv: {
	config?: #Config
	env?: #Env
	hooks?: #Hooks
	workspaces?: #Workspaces
	tasks: [string]: #Tasks | *{}
}
