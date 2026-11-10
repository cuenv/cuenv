package schema

// A named context is a filter over one invocation of cuenv. While the filter
// matches, `env.context.<name>` sets its variables.
//
// A filter is data: fields left out match anything, and a string is a regular
// expression that must match the whole value, so "bun" matches only "bun" and
// "deploy.*" matches every name that starts with "deploy". Write an entry as
// `schema.#When & {...}` or `schema.#TaskFilter & {...}`.
//
// Everything a filter reads is reported by the machine running cuenv. A
// context chooses what a run asks for. It does not establish that the run is
// entitled to it: the secrets backend must enforce that from claims it can
// verify.

// #When matches the facts of an invocation: what consumes the environment,
// where, and for whom. `null` matches a fact that does not apply.
#When: {
	// What consumes the environment: "task", "exec", "infrastructure" or
	// "shell".
	consumer?: string

	// The program being run, for `cuenv exec`, as it is written on the
	// command line ("terraform" and "/usr/bin/terraform" differ); null for
	// any other consumer.
	exec?: string | null

	// The infrastructure action (see #InfrastructureAction), for
	// `cuenv infrastructure`; null for any other consumer.
	infrastructure?: string | null

	// The CI run, or null outside CI.
	ci?: null | {
		// "github", "gitlab", "buildkite", or "unknown".
		provider?: string
		// The triggering event, such as "pull_request" or "push".
		event?: string | null
		// The ref as the provider names it: the full ref on GitHub
		// ("refs/heads/main"), the branch or tag name on GitLab and Buildkite.
		ref?: string | null
		// Who triggered the run.
		actor?: string | null
	}

	// The git identity of the person running cuenv.
	user?: {
		name?:  string | null
		email?: string | null
	}

	host?: {
		// "linux", "macos", "windows", ...
		os?: string
		// "x86_64", "aarch64", ...
		arch?: string
		name?: string | null
	}

	vcs?: {
		// The checked-out branch, or null when detached or outside a repository.
		branch?: string | null
	}
}

// #TaskFilter matches the tasks cuenv runs, one task at a time: the context
// is active for each task of the graph that the filter matches, and for
// nothing that is not a task.
#TaskFilter: {
	// The task's full name, such as "deploy" or "deploy.staging".
	name?: string

	// The program the task runs. A script task has none and never matches.
	command?: string

	// Labels the task must carry: each entry must match one of its labels.
	labels?: [...string]
}
