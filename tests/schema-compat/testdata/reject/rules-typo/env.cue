// want: ignoree: field not allowed
package compat

import "github.com/cuenv/cuenv/schema/rules"

rules.#DirectoryRules

ignore: git: ["target/"]

ignoree: git: ["target/"]
