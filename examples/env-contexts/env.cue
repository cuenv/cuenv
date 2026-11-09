package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "env-contexts"

// A context is a constraint over the facts of an invocation. cuenv decides
// which contexts are active each time it resolves an environment.
contexts: {
	plan:   when: infrastructure: "plan"
	apply:  when: infrastructure: "apply" | "import" | "destroy"
	deploy: when: task:           =~"^deploy"
	local:  when: ci:             null
}

env: {
	REGION: "eu-west-1"

	// A variable set under a context exists only while that context is
	// active, so the same name can carry a different value for each one.
	context: {
		plan: API_TOKEN:     "read-only"
		apply: API_TOKEN:    "read-write"
		deploy: DEPLOY_HOST: "deploy.example.com"
		local: EDITOR_HINT:  "running on a workstation"
	}
}

tasks: {
	build: {
		command: "sh"
		args: ["-c", "echo build sees DEPLOY_HOST=${DEPLOY_HOST:-unset}"]
	}
	"deploy": {
		command: "sh"
		args: ["-c", "echo deploy sees DEPLOY_HOST=${DEPLOY_HOST:-unset}"]
	}
}
