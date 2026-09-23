package schema

#Hooks: close({
	onEnter?: [string]: #Hook
	onExit?: [string]:  #Hook
	prePush?: [string]: #Hook
})

#Hook: #ExecHook

#ExecHook: close({
	order?:     int | *100
	propagate?: bool | *false
	command!:   string
	args?: [...string]
	dir?: string | *"."
	inputs?: [...string]
	// Evaluate stdout as a shell script and capture the resulting
	// environment. A hook whose output cannot be evaluated is reported as
	// failed even when the process exits 0.
	source?: bool
})
