package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project & {
	name: "vcs-subdir"

	vcs: "agent-skills": {
		url:       "https://github.com/cuenv/cuenv.git"
		reference: "main"
		vendor:    false
		subdir:    ".agents/skills"
		path:      ".agents/skills"
	}

	// The synced tree is what the task reads, so it is what the task
	// declares: a hermetic task sees only its inputs.
	tasks: inspect: schema.#Task & {
		command: "sh"
		args: [
			"-c",
			"find .agents/skills -maxdepth 2 -type f | sort | sed -n '1,10p'",
		]
		inputs: [".agents/skills/**"]
	}
}
