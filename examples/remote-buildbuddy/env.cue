package _examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "remote-buildbuddy"

config: {
	// Remote backend using BuildBuddy
	backend: {
		type: "remote"
		options: {
			// BuildBuddy endpoint (customize to your instance)
			endpoint: "grpcs://rawkode-academy.buildbuddy.io"

			// BuildBuddy authentication using 1Password
			// The API key is resolved at runtime via `op read`
			auth: {
				type: "buildbuddy"
				apiKey: schema.#OnePasswordRef & {
					ref: "op://Private/BuildBuddy/api-key"
				}
			}
		}
	}
}

tasks: {
	// Simple echo command to test remote execution
	"hello": {
		command:     "echo"
		args: ["Hello from BuildBuddy remote execution!"]
		description: "Basic hello world via remote execution"
	}

	// Test with environment variables
	"env-test": {
		command:     "sh"
		args: ["-c", "echo \"Running on: $(uname -a)\" && echo \"PWD: $PWD\""]
		description: "Show remote environment info"
	}

	// Test with inputs
	"checksum": {
		command:     "sha256sum"
		args: ["env.cue"]
		inputs: ["env.cue"]
		description: "Compute checksum of input file"
	}
}
