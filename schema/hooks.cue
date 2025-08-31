package schema

#Hooks: {
	onEnter?: #Hook | [...#Hook]
	onExit?: #Hook | [...#Hook]
}

#Hook: #ExecHook

#ExecHook: {
	command!: string
	args?: [...string]
	dir?: string | *"."
	inputs?: [...string]
	source?: bool
	preload?: bool | *false

	// To be extended
	...
}

