package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "dagger-task"

config: {
	// Default Dagger backend for all tasks
	backend: {
		type: "dagger"
		options: {
			image: "alpine:latest"
		}
	}
}

tasks: {
	"hello": {
		command:     "hostname"
		description: "Basic hello world in an Alpine container, using global backend."
	}

	// Run Python script in Python container
	"python-info": {
		command:     "python"
		args: ["-c", "import sys; print(f'Running Python {sys.version} in Dagger')"]
		description: "Show Python version in a Python container"
		dagger: {
			image: "python:3.11-slim"
		}
	}

	// ==========================================================================
	// Container Chaining Examples
	// ==========================================================================

	// Stage 1: Install packages into the container
	"stage1.setup": {
		command:     "sh"
		args: ["-c", "apk add --no-cache curl jq && echo 'Setup complete!'"]
		description: "Install curl and jq into Alpine container"
		dagger: {
			image: "alpine:latest"
		}
	}

	// Stage 2: Continue from stage1's container (has curl and jq installed)
	"stage2.use-tools": {
		command:     "sh"
		args: ["-c", "echo 'Tools available:' && which curl && which jq && echo '{\"test\": 123}' | jq ."]
		description: "Use tools installed in stage1"
		dependsOn: [tasks["stage1.setup"]]
		dagger: {
			from: "stage1.setup" // Continue from previous container state
		}
	}

	// ==========================================================================
	// Cache Volume Examples
	// ==========================================================================

	// Example with cache volumes for package managers
	"cached-install": {
		command:     "sh"
		args: ["-c", "pip install requests && python -c 'import requests; print(requests.__version__)'"]
		description: "Install Python package with pip cache"
		dagger: {
			image: "python:3.11-slim"
			cache: [
				{path: "/root/.cache/pip", name: "pip-cache"},
			]
		}
	}

	// ==========================================================================
	// Secret Examples
	// ==========================================================================

	// Example: Mount a secret as environment variable
	// Uses exec resolver to get secret from a command
	"secret-env-example": {
		command:     "sh"
		args: ["-c", "echo 'API Token length:' && echo -n $API_TOKEN | wc -c"]
		description: "Demonstrate secret as environment variable"
		dagger: {
			image: "alpine:latest"
			secrets: [
				{
					name:   "api-token"
					envVar: "API_TOKEN"
					resolver: {
						resolver: "exec"
						command:  "echo"
						args: ["my-secret-token-12345"]
					}
				},
			]
		}
	}

	// Example: Mount a secret as a file
	"secret-file-example": {
		command:     "sh"
		args: ["-c", "echo 'Secret file contents:' && cat /run/secrets/config.json | head -c 50"]
		description: "Demonstrate secret as mounted file"
		dagger: {
			image: "alpine:latest"
			secrets: [
				{
					name: "config-secret"
					path: "/run/secrets/config.json"
					resolver: {
						resolver: "exec"
						command:  "echo"
						args: ["{\"key\": \"value\", \"nested\": {\"data\": \"secret\"}}"]
					}
				},
			]
		}
	}

	// ==========================================================================
	// Combined Example: Multi-stage build with cache and secrets
	// ==========================================================================

	// Stage 1: Install dependencies with caching
	"build.deps": {
		command:     "sh"
		args: ["-c", "pip install flask gunicorn && pip freeze > /workspace/requirements.txt"]
		description: "Install Python dependencies with pip cache"
		outputs: ["requirements.txt"]
		dagger: {
			image: "python:3.11-slim"
			cache: [
				{path: "/root/.cache/pip", name: "pip-cache"},
			]
		}
	}

	// Stage 2: Continue and run tests
	"build.test": {
		command:     "sh"
		args: ["-c", "python -c 'import flask; import gunicorn; print(\"All imports OK\")'"]
		description: "Verify dependencies are installed"
		dependsOn: [tasks["build.deps"]]
		inputs: [{task: "build.deps"}]
		dagger: {
			from: "build.deps" // Continue from deps container
		}
	}

	// Stage 3: Final verification
	"build.verify": {
		command:     "sh"
		args: ["-c", "cat /workspace/requirements.txt && echo '---' && python --version"]
		description: "Show final build artifacts"
		dependsOn: [tasks["build.test"]]
		inputs: [{task: "build.deps"}]
		dagger: {
			from: "build.test" // Continue from test container
		}
	}
}
