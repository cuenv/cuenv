package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "accept"

runtime: schema.#DaggerRuntime & {
	image:   "rust:1.100-slim"
	workdir: "/work"
	cacheVolumes: [
		{key: "cargo-registry", path: "/usr/local/cargo/registry"},
	]
}

services: {
	db: schema.#Service & {
		entrypoint: schema.#Command & {
			command: "postgres"
		}
		ports: [{name: "pg", port: 5432}]
	}
}

let _services = services
let _tasks = tasks

tasks: {
	test: schema.#Task & {
		command: "cargo"
		args: ["test"]
		dependsOn: [_services.db]
	}
	checks: schema.#TaskGroup & {
		dependsOn: [_services.db]
		ping: schema.#Task & {
			command: "true"
		}
	}
}

ci: {
	providers: ["dagger"]
	provider: dagger: {
		defaultImage: "rust:1.100-slim"
		moduleName:   "accept"
	}
	pipelines: default: tasks: [_tasks.test]
}
