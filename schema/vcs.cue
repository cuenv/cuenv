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
	// true copies a tracked snapshot; false writes generated content ignored by git.
	// Full-repo non-vendored dependencies keep .git metadata as local checkouts.
	vendor!: bool
	// Repository-relative materialization path.
	path!: string
	// Optional repo-relative subdirectory to materialize via sparse checkout.
	// When set, only this subtree is materialized at `path`.
	// Must be a forward-slash relative path with no ".", "..", or glob characters.
	subdir?: string
})
