package examples

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "ci-pipeline"

ci: pipelines: {
    default: {
        tasks: ["test"]
    }
}

tasks: {
    test: schema.#Task & {
        command: "echo"
        args: ["Running test task"]
        inputs: ["env.cue"]
    }
}
