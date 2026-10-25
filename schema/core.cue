package schema

// Closedness. Consumers embed `schema.#Project` (or `schema.#Base`) at file
// level. Before CUE language v0.18 that leaves the file's own struct open,
// and a field typed with a bare definition reference (`config?: #Config`)
// is left open with it. Referencing the definition inside a struct literal
// (`config?: {#Config}`) keeps it closed under every CUE release and
// language version, so a misspelled nested field is an error. Definitions
// are already closed, so the schema never wraps them in close(): on CUE
// v0.17+ close() around an embedded definition rejects the definition's own
// fields under file-level embedding. The inline passthrough arm of
// #InfrastructureStateToken is the exception: it is not a definition, and an
// open arm would accept extra fields. scripts/check-schema-compat.sh checks
// both properties.
//
// Exception: the `tasks`, `services` and `images` maps reference #Task,
// #TaskGroup, #Service and #ContainerImage directly. `dependsOn` and pipeline
// task lists are typed references to these entries, and cuenv's CUE fork
// shares such a reference only when the entry was constrained by a direct
// definition reference; inside a struct literal the sharing is lost and
// evaluation becomes exponential in the depth of the dependency graph (see
// docs/design/specs/cue-recursive-sharing.md). The cost is that, before
// language v0.18, a misspelled top-level field of an entry written without
// `schema.#Task &` (or `schema.#Service &`, `schema.#ContainerImage &`) is
// ignored. Nested fields are still checked.
#Base: {
	config?: {#Config}
	env?: {#Env}
	formatters?: {#Formatters}
	runtime?: #Runtime
	hooks?: {#Hooks}
	vcs?: [#VcsDependencyName]: {#VcsDependency}
}

#ProjectName: string & =~"^[a-zA-Z0-9._-]+$"

#VcsDependencyName: string & =~"^[a-zA-Z0-9_-][a-zA-Z0-9._-]*$" & !~"\\.\\."

#Project: {
	#Base
	name!: #ProjectName
	ci?: {#CI}
	cache?: {#Cache}
	release?: {#Release}
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
	codegen?: {#Codegen}
	// Infrastructure managed through Terraform provider plugins.
	infrastructure?: {#Infrastructure}
}
