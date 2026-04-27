package schema

#Base: close({
	config?:     #Config
	env?:        #Env
	formatters?: #Formatters
	runtime?:    #Runtime
	hooks?:      #Hooks
	vcs?:        [#VcsDependencyName]: #VcsDependency
})

#ProjectName: string & =~"^[a-zA-Z0-9._-]+$"

#VcsDependencyName: string & =~"^[a-zA-Z0-9_-][a-zA-Z0-9._-]*$" & !~"\\.\\."

#Project: close({
	#Base
	name!:    #ProjectName
	runtime?: #Runtime
	hooks?:   #Hooks
	vcs?:     [#VcsDependencyName]: #VcsDependency
	ci?:      #CI
	release?: #Release
	// Named tasks and groups derive fully-qualified runtime names from their
	// field labels. Sequences are handled by the Go bridge because list element
	// aliases are not implemented in CUE yet.
	tasks?: [taskName=string]: ((#Task | #TaskGroup) & {
		_cuenvPrefix: ""
		_cuenvSelf:   taskName
	}) | #TaskSequence
	// Services live in their own field but share the project DAG.
	// Services may depend on tasks (build → run); tasks may depend on
	// services (db ready → seed task). Cycles forbidden as with tasks.
	services?: [svcName=string]: #Service & {
		_cuenvPrefix: ""
		_cuenvSelf:   svcName
	}
	// Container image builds — declarative image definitions that
	// participate in the task DAG and produce output references
	// (ref, digest) consumable by tasks and other images.
	images?: [imageName=string]: #ContainerImage & {
		_cuenvPrefix: ""
		_cuenvSelf:   imageName
	}
	codegen?: #Codegen
})

// Match tasks across projects by metadata for discovery-based execution.
#TaskMatcher: close({
	// Match tasks with these labels (all must match)
	labels?: [...string]

	// Match tasks whose command matches this value
	command?: string

	// Match tasks whose args contain specific patterns
	args?: [...#ArgMatcher]

	// Run matched tasks in parallel (default: true)
	parallel: bool | *true
})

// Discovery-based hook step that expands a #TaskMatcher into tasks.
#MatchHook: close({
	// Optional stable name used for task naming/logging
	name?: string
	// Task matcher to select tasks across the workspace
	match!: #TaskMatcher
})

// Pattern matcher for task arguments
#ArgMatcher: close({
	// Match if any arg contains this substring
	contains?: string
	// Match if any arg matches this regex pattern
	matches?: string
})
