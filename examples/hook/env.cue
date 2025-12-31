package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "hook"

// Environment variables to be loaded after hooks complete
env: {
	CUENV_TEST:    "loaded_successfully"
	API_ENDPOINT:  "http://localhost:8080/api"
	DEBUG_MODE:    "true"
	PROJECT_NAME:  "hook-example"
}

// Hooks to execute when entering this directory
hooks: onEnter: notify: { command: "echo", args: ["Environment configured"] }

// Task definitions for the environment
tasks: {
	verify_env: {
		command: "sh"
		args: ["-c", "echo CUENV_TEST=$CUENV_TEST API_ENDPOINT=$API_ENDPOINT"]
	}
	
	show_env: {
		command: "sh"
		args: ["-c", "env | grep CUENV"]
	}
}
