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

	tasks: inspect: schema.#Task & {
		command: "sh"
		args: [
			"-c",
			"find .agents/skills -maxdepth 2 -type f | sort | sed -n '1,10p'",
		]
	}
}
