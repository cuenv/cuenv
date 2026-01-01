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
	source?: bool
})
