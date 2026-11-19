package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "dagger-basic"

runtime: schema.#DaggerRuntime & {
	image:   "rust:1.100-slim"
	workdir: "/work"
	cacheVolumes: [
		{key: "cargo-registry", path: "/usr/local/cargo/registry", sharing: "SHARED"},
		{key: "cargo-target", path: "/work/target", sharing: "LOCKED"},
	]
}

services: {
	postgres: schema.#Service & {
		entrypoint: schema.#Command & {
			command: "postgres"
			args: ["-c", "listen_addresses=*"]
		}
		ports: [{name: "pg", port: 5432}]
		readiness: {
			kind: "port"
			port: 5432
		}
	}
}

let _services = services
let _tasks = tasks

tasks: {
	build: schema.#Task & {
		command: "cargo"
		args: ["build", "--workspace"]
		inputs: ["Cargo.toml", "Cargo.lock", "src/**"]
		outputs: ["target/debug/app"]
	}

	integration: schema.#Task & {
		command: "cargo"
		args: ["test", "--test", "integration"]
		dependsOn: [_tasks.build, _services.postgres]
	}
}

ci: {
	providers: ["dagger"]
	provider: dagger: {
		defaultImage: "rust:1.100-slim"
		moduleName:   "dagger-basic"
	}
	pipelines: {
		default: {
			mode: "expanded"
			tasks: [_tasks.build, _tasks.integration]
		}
	}
}
