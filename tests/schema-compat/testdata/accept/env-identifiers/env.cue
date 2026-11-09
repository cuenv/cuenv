package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "accept"

env: {
	FOO:        "x"
	FOO_1:      "y"
	"_LEADING": "z"
	environment: production: BAR_2: "z"
}
