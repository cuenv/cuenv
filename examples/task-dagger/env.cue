package examples

import "github.com/cuenv/cuenv/schema"

schema.#Cuenv

// Configure Dagger as the default backend for task execution
config: {
	backend: {
		type: "dagger"
		options: {
			// Default container image for all tasks
			image: "ubuntu:22.04"
		}
	}
}

env: {
	GREETING: "Hello from Dagger"
}

tasks: {
	// Simple task that runs in a Dagger container
	hello: {
		command:     "echo"
		args:        [env.GREETING]
		description: "Say hello from inside a Dagger container"
	}

	// Task with custom container image (overrides the global default)
	alpine_hello: {
		command:     "echo"
		args:        ["Hello from Alpine"]
		description: "Run in an Alpine container"
		dagger: {
			image: "alpine:latest"
		}
	}

	// Build task that demonstrates container-based builds
	build: {
		command:     "sh"
		args:        ["-c", "echo Building... && echo BUILD_ID=$(date +%s) > /tmp/build-output"]
		description: "Simulated build task running in container"
	}

	// Multi-step workflow
	ci: {
		lint: {
			command:     "sh"
			args:        ["-c", "echo Linting..."]
			description: "Run linting in container"
		}
		test: {
			command:     "sh"
			args:        ["-c", "echo Testing..."]
			description: "Run tests in container"
		}
	}
}
