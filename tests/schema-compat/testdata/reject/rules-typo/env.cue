// want: ignoree: field not allowed
// File-level embedding, the shape examples/env-basic/.rules.cue uses.
// Before min-language a top-level typo is accepted. The compat check requires that.
package compat

import "github.com/cuenv/cuenv/schema/rules"

rules.#DirectoryRules

ignore: git: ["target/"]

ignoree: git: ["target/"]
