package examples

import "github.com/cuenv/cuenv/schema"

schema.#Cuenv

env: {
    SHOULD_NOT_LOAD: "this_should_not_be_set"
}

hooks: {
    onEnter: fail: {
        command: "sh"
        args: ["-c", "exit 1"]
    }
}
