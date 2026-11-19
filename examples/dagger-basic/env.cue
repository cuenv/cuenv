package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "dagger-basic"

runtime: schema.#DaggerRuntime & {
	image:   "postgres:17-alpine"
	workdir: "/work"
}

services: {
	postgres: schema.#Service & {
		entrypoint: schema.#Command & {
			command: "docker-entrypoint.sh"
			args: ["postgres", "-c", "listen_addresses=*"]
		}
		runtime: schema.#ContainerRuntime & {
			image: "postgres:17-alpine"
		}
		env: {
			POSTGRES_HOST_AUTH_METHOD: "trust"
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
		command: "sh"
		args: ["-ec", "mkdir -p build; printf 'SELECT 1;\\n' > build/query.sql"]
		outputs: ["build/query.sql"]
	}

	integration: schema.#Task & {
		command: "psql"
		args: ["-h", "postgres", "-U", "postgres", "-v", "ON_ERROR_STOP=1", "-f", "build/query.sql"]
		dependsOn: [_tasks.build, _services.postgres]
		inputs: [_tasks.build.outputFiles]
	}
}

ci: {
	providers: ["dagger"]
	provider: dagger: {
		defaultImage: "postgres:17-alpine"
		moduleName:   "dagger-basic"
	}
	pipelines: {
		default: {
			mode: "expanded"
			tasks: [_tasks.build, _tasks.integration]
		}
	}
}
