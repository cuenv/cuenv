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
    test: {
        command: "echo"
        args: ["Running test task"]
        inputs: ["env.cue"]
    }
}
