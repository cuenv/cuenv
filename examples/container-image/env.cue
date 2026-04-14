package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "container-image-example"

tasks: {
	codegen: schema.#Task & {
		command:  "echo"
		args: ["generating proto files"]
		hermetic: false
	}
}

images: {
	api: schema.#ContainerImage & {
		context:    "."
		dockerfile: "Dockerfile"
		tags: ["latest", "v1.0.0"]
		dependsOn: [tasks.codegen]
		inputs: ["src/**", "Dockerfile"]
		description: "API server container image"
	}

	worker: schema.#ContainerImage & {
		context: "."
		target:  "worker"
		tags: ["latest"]
		labels: ["ci"]
		platform: ["linux/amd64", "linux/arm64"]
		description: "Background worker image"
	}
}
