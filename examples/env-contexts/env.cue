package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "env-contexts"

// A context is a filter over an invocation. cuenv decides which contexts are
// active each time it resolves an environment. A string is a regular
// expression that must match the whole value.
contexts: {
	plan: schema.#When & {infrastructure: "plan"}
	apply: schema.#When & {infrastructure: "apply|import|destroy"}
	local: schema.#When & {ci: null}
	deploy: schema.#TaskFilter & {name: "deploy.*"}
	release: schema.#TaskFilter & {command: "sh", labels: ["release"]}
}

env: {
	REGION: "eu-west-1"

	// A variable set under a context exists only while that context is
	// active, so the same name can carry a different value for each one.
	context: {
		plan: API_TOKEN:       "read-only"
		apply: API_TOKEN:      "read-write"
		local: EDITOR_HINT:    "running on a workstation"
		deploy: DEPLOY_HOST:   "deploy.example.com"
		release: RELEASE_NOTE: "set for sh tasks labelled release"
	}
}

tasks: {
	build: schema.#Task & {
		command: "sh"
		args: ["-c", "echo build sees DEPLOY_HOST=${DEPLOY_HOST:-unset}"]
	}
	"deploy": schema.#Task & {
		command: "sh"
		args: ["-c", "echo deploy sees DEPLOY_HOST=${DEPLOY_HOST:-unset}"]
		dependsOn: [build]
	}
	publish: schema.#Task & {
		command: "sh"
		args: ["-c", "echo publish sees RELEASE_NOTE=${RELEASE_NOTE:-unset}"]
		labels: ["release"]
	}
}
