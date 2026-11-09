package schema

// #Context describes one invocation of cuenv: what is being run, where, and
// by whom. cuenv builds one for every consumer of the environment (each task
// of `cuenv task`, the command of `cuenv exec`, an infrastructure action).
// Every field is always present; a fact that does not apply is null.
//
// Every fact is reported by the machine running cuenv. A context chooses what
// a run asks for. It does not establish that the run is entitled to it: the
// secrets backend must enforce that from claims it can verify.
#Context: {
	// The cuenv command that consumes the environment.
	command: "task" | "exec" | "infrastructure" | "shell"

	// The task's name, for `command: "task"`.
	task: string | null

	// The program being run, for `command: "exec"`.
	exec: string | null

	// The infrastructure action, for `command: "infrastructure"`.
	infrastructure: #InfrastructureAction | null

	// The CI run, or null outside CI.
	ci: null | {
		// "github", "gitlab", "buildkite", or "unknown".
		provider: string
		// The triggering event, such as "pull_request" or "push".
		event: string | null
		// The full ref, such as "refs/heads/main".
		ref: string | null
		// Who triggered the run.
		actor: string | null
	}

	// The git identity of the person running cuenv.
	user: {
		name:  string | null
		email: string | null
	}

	host: {
		// "linux", "macos", "windows", ...
		os: string
		// "x86_64", "aarch64", ...
		arch: string
		name: string | null
	}

	vcs: {
		// The checked-out branch, or null when detached or outside a repository.
		branch: string | null
	}
}

// #NamedContext is a constraint over #Context. The context is active for an
// invocation whose facts satisfy `when`.
//
// `when` may use literals, `=~`, `!~`, `!=`, `|` and `&`. Fields left out
// match anything.
#NamedContext: {
	when: #Context
}
