package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "services-readiness"

tasks: {
	prepare: schema.#Task & {
		command: "sh"
		args: ["-c", "mkdir -p .cuenv/run && echo prepared > .cuenv/run/services-readiness.txt"]
	}
}

services: {
	port: schema.#Service & {
		dependsOn: [tasks.prepare]
		entrypoint: {
			command: "python3"
			args: ["-c", "import socket, time; s=socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1); s.bind(('127.0.0.1', 18080)); s.listen(1); time.sleep(3600)"]
		}
		readiness: {
			kind: "port"
			port: 18080
		}
		shutdown: {timeout: "2s"}
	}

	http: schema.#Service & {
		entrypoint: {
			command: "python3"
			args: ["-m", "http.server", "18081", "--bind", "127.0.0.1"]
		}
		readiness: {
			kind: "http"
			url:  "http://127.0.0.1:18081/"
		}
		shutdown: {timeout: "2s"}
	}

	log: schema.#Service & {
		entrypoint: {
			command: "sh"
			args: ["-c", "echo 'worker ready'; while :; do sleep 60; done"]
		}
		readiness: {
			kind:    "log"
			pattern: "worker ready"
			source:  "stdout"
		}
		logs: {
			prefix: "log-probe"
			persist: true
		}
		shutdown: {timeout: "2s"}
	}

	command: schema.#Service & {
		entrypoint: {
			command: "sh"
			args: ["-c", "touch .cuenv-command-ready; while :; do sleep 60; done"]
		}
		readiness: {
			kind:    "command"
			command: "test"
			args:    ["-f", ".cuenv-command-ready"]
		}
		shutdown: {timeout: "2s"}
	}

	delay: schema.#Service & {
		entrypoint: {
			command: "sh"
			args: ["-c", "while :; do sleep 60; done"]
		}
		readiness: {
			kind:  "delay"
			delay: "1s"
		}
		restart: {
			mode:        "unlessStopped"
			maxRestarts: 3
			window:      "30s"
		}
		watch: {
			paths: ["env.cue"]
			on:    "restart"
		}
		shutdown: {timeout: "2s"}
	}
}
