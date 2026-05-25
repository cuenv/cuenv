package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

#Service: {
	tasks: {
		migrate: schema.#Task & {
			command: "echo"
			args: ["migrate"]
		}
		deploy: schema.#Task & {
			command: "echo"
			args: ["deploy"]
			dependsOn: [migrate]
		}
	}
	_tasks: tasks
	ci: pipelines: {
		default: {
			tasks: [_tasks.deploy]
		}
	}
}

_svc: #Service

name: "reusable-task-refs"
tasks: _svc.tasks
ci: _svc.ci
