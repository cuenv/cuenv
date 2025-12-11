package examples

import "github.com/cuenv/cuenv/schema"

schema.#Cuenv

_ci: pipelines: [
    {
        name: "default"
        tasks: ["test"]
    }
]

tasks: {
    test: {
        command: "echo"
        args: ["Running test task"]
        inputs: ["env.cue"]
    }
}
