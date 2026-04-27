package schema

// #VcsDependency declares a cuenv-managed Git dependency.
//
// `vendor` and `path` are required so projects make an explicit choice between
// tracked source snapshots and local generated checkouts.
#VcsDependency: close({
	// Git repository URL. Local paths are accepted by Git for tests and private mirrors.
	url!: string
	// Branch, tag, or commit-ish to resolve. Defaults to the remote default branch.
	reference: string | *"HEAD"
	// true copies a tracked snapshot without .git metadata; false writes a local checkout.
	vendor!: bool
	// Repository-relative materialization path.
	path!: string
})
