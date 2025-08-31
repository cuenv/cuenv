package schema

#Cuenv: {
	config?: #Config
	capabilities?: [string]: #Capability
	env?: #Env
	hooks?: #Hooks
	tasks: [string]: #Tasks | *{}
}
