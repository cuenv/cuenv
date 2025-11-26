package schema

#Hooks: {
	onEnter?: [string]: #Hook
	onExit?:  [string]: #Hook
}

#Hook: #ExecHook

#ExecHook: {
	order?:     int | *100
	propagate?: bool | *false
	command!:   string
	args?: [...string]
	dir?:    string | *"."
	inputs?: [...string]
	source?: bool

	// To be extended
	...
}

