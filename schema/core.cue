package schema

// Project files embed `schema.#Project` as a closed definition so CUE rejects
// unknown fields. A shared base embeds `schema.#Base...` because the projects
// inheriting that base declare their own fields.
//
// The task, service and image maps reference definitions directly so cuenv's
// CUE fork can share typed references to entries in `dependsOn` and pipeline
// task lists. Wrapping those references in a struct literal loses that
// sharing (see docs/design/specs/cue-recursive-sharing.md).
#Base: {
	config?: {#Config}
	env?: {#Env}
	// Named contexts, selected automatically for each invocation.
	// `env.context.<name>` overrides variables while the context is active.
	contexts?: [string]: {#When} | {#TaskFilter}
	formatters?: {#Formatters}
	runtime?: #Runtime
	hooks?: {#Hooks}
	vcs?: [#VcsDependencyName]: {#VcsDependency}
}

#ProjectName: string & =~"^[a-zA-Z0-9._-]+$"

#VcsDependencyName: string & =~"^[a-zA-Z0-9_-][a-zA-Z0-9._-]*$" & !~"\\.\\."

#Project: {
	#Base...
	name!: #ProjectName
	// Files of this project, chosen by whoever imports it:
	// `<pkg>.#Files & {paths: ["articles/**"]}`. Paths are relative to this
	// project's directory. A #Base has no #Files: it is merged into every
	// project below it and owns no directory of its own; files beside one are
	// named with #ModuleFiles.
	#Files: {#ProjectFiles, cuenvProject: name}
	ci?: {#CI}
	cache?: {#Cache}
	release?: {#Release}
	// Named tasks and groups derive fully-qualified runtime names from their
	// field labels. Sequences are handled by the Go bridge because list element
	// aliases are not implemented in CUE yet.
	// Every task, group, service and image is stamped with the project it
	// belongs to, so a reference to one from another project carries where it
	// came from in its value.
	tasks?: [string]~(taskName,_): ((#Task | #TaskGroup) & {
		_cuenvPrefix:  ""
		_cuenvSelf:    taskName
		_cuenvProject: name
	}) | (#TaskSequence & [...({_cuenvProject: name, ...} | [...])])
	// Directories whose script files become tasks, named after each file.
	// Nothing is scanned unless a directory is listed here.
	taskScripts?: [...#TaskScriptDirectory]
	// Services live in their own field but share the project DAG.
	// Services may depend on tasks (build → run); tasks may depend on
	// services (db ready → seed task). Cycles forbidden as with tasks.
	services?: [string]~(svcName,_): #Service & {
		_cuenvPrefix:  ""
		_cuenvSelf:    svcName
		_cuenvProject: name
	}
	// Container image builds — declarative image definitions that
	// participate in the task DAG and produce output references
	// (ref, digest) consumable by tasks and other images.
	images?: [string]~(imageName,_): #ContainerImage & {
		_cuenvPrefix:  ""
		_cuenvSelf:    imageName
		_cuenvProject: name
	}
	codegen?: {#Codegen}
	// Infrastructure managed through Terraform provider plugins.
	infrastructure?: {#Infrastructure}
}
