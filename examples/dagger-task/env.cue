package examples

import "github.com/cuenv/cuenv/schema"

schema.#Cuenv

config: {
	// You can set the default backend globally:
	// backend: {
	// 	type: "dagger"
	// 	options: {
	// 		image: "alpine:latest"
	// 	}
	// }
}

tasks: {
	// Simple echo in Alpine
	"hello": {
		command: "hostname"
		dagger: {
			image: "alpine:latest"
		}
	}

	// Run Python script in Python container
	"python-info": {
		command: "python"
		args: ["-c", "import sys; print(f'Running Python {sys.version} in Dagger')"]
		dagger: {
			image: "python:3.11-slim"
		}
	}

	// Demonstrate using environment variables in container
	"env-check": {
		command: "sh"
		args: ["-c", "echo \"The secret is: $MY_SECRET\""]
		env: {
			"MY_SECRET": "visible-in-container"
		}
		dagger: {
			image: "alpine:latest"
		}
	}
}
