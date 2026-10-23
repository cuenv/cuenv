// want: conflicting values
package compat

import "github.com/cuenv/cuenv/schema"

schema.#Project

name: "reject"

ci: contributors: [{
	id: "x"
	tasks: [{
		id:      "t"
		command: "x"
		provider: github: {uses: "actions/setup@v1", with: matrix: {os: "linux"}}
	}]
}]
